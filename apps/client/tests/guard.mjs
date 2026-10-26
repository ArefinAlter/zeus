// Stops what a failed acceptance run started (the page, the engine and its zeusd, the fixture server), so a crash in
// a script never leaves a daemon behind. `get` returns the current { h, fx, ui }.
export function guard(get) {
  const bail = async (e) => {
    console.error(e)
    try {
      const { h, fx, ui } = get()
      await ui?.close()
      await h?.stop()
      await fx?.close()
    } catch {
      // Best effort.
    }
    process.exit(1)
  }
  process.on('uncaughtException', bail)
  process.on('unhandledRejection', bail)
}
