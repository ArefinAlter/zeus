# Exercise the real Windows ACL and native OpenSSH behavior without any account secret.
#requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$runnerSid = $identity.User.Value
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('zeus-ci-acl-' + [Guid]::NewGuid().ToString('N'))
$sshKeygen = Join-Path $env:WINDIR 'System32\OpenSSH\ssh-keygen.exe'
$icacls = Join-Path $env:WINDIR 'System32\icacls.exe'
$node = (Get-Command node.exe -ErrorAction Stop).Source

function Assert-AllowedPrincipals([string] $Target, [bool] $ExpectProtected) {
    $acl = Get-Acl -LiteralPath $Target
    if ($ExpectProtected -and -not $acl.AreAccessRulesProtected) { throw 'Expected a protected DACL.' }
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if ($rules.Count -eq 0) { throw 'Expected nonempty access rules.' }
    foreach ($rule in $rules) {
        if (@($runnerSid, 'S-1-5-18') -notcontains $rule.IdentityReference.Value -or
            $rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) {
            throw 'Unexpected principal or deny rule in the test ACL.'
        }
    }
}

function Run-CheckedProcess([string] $Executable, [string] $Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Executable
    $start.Arguments = $Arguments
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($start)
    $output = $process.StandardOutput.ReadToEnd()
    $errorText = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Fixture process failed: $errorText" }
    $process.Dispose()
    return $output
}

try {
    $null = New-Item -ItemType Directory -Path $testRoot
    # Reproduce an unsafe runner parent without touching the real RUNNER_TEMP.
    $unsafeAcl = Get-Acl -LiteralPath $testRoot
    $unsafeAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        [Security.Principal.SecurityIdentifier]::new('S-1-5-11'),
        [Security.AccessControl.FileSystemRights]::FullControl,
        ([Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit),
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Allow
    ))
    Set-Acl -LiteralPath $testRoot -AclObject $unsafeAcl
    $before = Get-Acl -LiteralPath $testRoot
    if (@($before.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]) |
        Where-Object { $_.IdentityReference.Value -eq 'S-1-5-11' }).Count -eq 0) {
        throw 'The unsafe-parent test precondition was not established.'
    }
    & (Join-Path $PSScriptRoot 'protect-runner-temp.ps1') -Path $testRoot
    Assert-AllowedPrincipals $testRoot $true

    # This key is newly generated, never registered with a provider, and deleted below.
    $fixtureKey = Join-Path $testRoot 'fixture-ed25519'
    $null = Run-CheckedProcess $sshKeygen ('-q -t ed25519 -N "" -C ci-acl-fixture -f "' + $fixtureKey + '"')
    $writer = Join-Path $testRoot 'node-writer.mjs'
    [IO.File]::WriteAllText($writer, 'import fs from "node:fs"; fs.writeFileSync(process.argv[3], fs.readFileSync(process.argv[2]), {mode: 0o600, flag: "wx"});')
    $checkoutKey = Join-Path $testRoot 'checkout-created-key'
    $null = Run-CheckedProcess $node ('"' + $writer + '" "' + $fixtureKey + '" "' + $checkoutKey + '"')
    Assert-AllowedPrincipals $checkoutKey $false

    # Match actions/checkout's Windows key hardening, including inheritance removal.
    $null = Run-CheckedProcess $icacls ('"' + $checkoutKey + '" /grant:r "' + $identity.Name + ':F"')
    $null = Run-CheckedProcess $icacls ('"' + $checkoutKey + '" /inheritance:r')
    Assert-AllowedPrincipals $checkoutKey $true
    $public = Run-CheckedProcess $sshKeygen ('-y -f "' + $checkoutKey + '"')
    if (-not $public.StartsWith('ssh-ed25519 ')) { throw 'Native OpenSSH did not read the generated key.' }
    Write-Host 'PASS: unsafe parent replaced; Node child and checkout ACL commands remain private; native OpenSSH accepts throwaway key.'
} finally {
    if (Test-Path -LiteralPath $testRoot) { Remove-Item -LiteralPath $testRoot -Recurse -Force }
}
