# The native Windows OpenSSH client rejects keys readable by other users. Checkout
# creates its temporary key with Node, so constrain inherited access before creation.
# Existing children are intentionally untouched; this is not a recursive ACL rewrite.
#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Path
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'Runner temp ACL protection requires Windows.'
}
$directory = Get-Item -LiteralPath $Path -Force
if (-not $directory.PSIsContainer -or
    ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'Runner temp must be an existing ordinary directory.'
}
$fullPath = $directory.FullName.TrimEnd([IO.Path]::DirectorySeparatorChar)
$rootPath = [IO.Path]::GetPathRoot($directory.FullName).TrimEnd([IO.Path]::DirectorySeparatorChar)
if ($fullPath -eq $rootPath) {
    throw 'Refusing to replace the ACL on a filesystem root.'
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$runnerSid = $identity.User
if ($null -eq $runnerSid) { throw 'Cannot resolve the current runner SID.' }
$systemSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
$inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
    [Security.AccessControl.InheritanceFlags]::ObjectInherit
$security = [Security.AccessControl.DirectorySecurity]::new()
$security.SetOwner($runnerSid)
$security.SetAccessRuleProtection($true, $false)
foreach ($sid in @($runnerSid, $systemSid)) {
    $security.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        $sid,
        [Security.AccessControl.FileSystemRights]::FullControl,
        $inheritance,
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Allow
    ))
}
Set-Acl -LiteralPath $directory.FullName -AclObject $security

$actual = Get-Acl -LiteralPath $directory.FullName
if (-not $actual.AreAccessRulesProtected -or
    $actual.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $runnerSid.Value) {
    throw 'Runner temp ACL protection or ownership did not take effect.'
}
$allowed = @($runnerSid.Value, $systemSid.Value)
$seen = @()
foreach ($rule in $actual.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
    if ($rule.IsInherited -or $rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or
        $allowed -notcontains $rule.IdentityReference.Value -or
        $rule.FileSystemRights -ne [Security.AccessControl.FileSystemRights]::FullControl -or
        $rule.InheritanceFlags -ne $inheritance -or
        $rule.PropagationFlags -ne [Security.AccessControl.PropagationFlags]::None) {
        throw 'Runner temp retained an unexpected access rule.'
    }
    $seen += $rule.IdentityReference.Value
}
foreach ($sid in $allowed) {
    if ($seen -notcontains $sid) { throw 'Runner temp is missing an expected access rule.' }
}
Write-Host 'Protected runner temp: inheritable access is limited to the runner SID and SYSTEM.'
