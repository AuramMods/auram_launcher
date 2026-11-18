# Native game branding

The launcher reads the managed client pack's `minecraft/branding/branding.json`
when constructing Minecraft's JVM argument file. Its relative icon and startup
agent paths are interpreted inside the branding directory. A missing descriptor
uses the normal Minecraft launch without agent options. Once a descriptor names
an agent, a read error or JVM initialization failure is reported normally.
Branding assets stay with the client pack and are excluded from server builds.

On macOS, Tauri packages the signed game helper at
`Contents/Helpers/Auram.app`. The helper presents its own name and unicorn icon
through AppKit, then loads the selected Zulu JLI by absolute path and preserves
first-thread GLFW initialization. It prepares the matching managed GLFW native
with the Cocoa working-directory hint disabled. Development builds use a
content-addressed copy of the signed helper embedded by Cargo. The helper is
signed ad hoc by default. For an Apple distribution build, have the signing
identity available in the keychain and set `APPLE_SIGNING_IDENTITY` before packaging.

On Windows, the existing UTF-8 JLI host accepts a dedicated game pipe command.
It assigns the game identity `art.arcane.auram.game` before starting Java. The
same executable contains the named `GLFW_ICON` and uses `Auram` for both the
product name and file description. The normal launcher entry point remains
available without a helper argument. Auram does not install a pinned shortcut
for the game helper. Java utilities, Forge installers and all server commands
keep their existing launch paths.

Run `pnpm icons` after changing `assets/icon.png` to regenerate the native
game icons. The source PNG also ships inside the Mac helper so AppKit can load
its pixels before GLFW creates a window.

The fixture at `scripts/fixtures/branding/AuramProbe.java` uses the actual
managed JVM, LWJGL, and AuramCore startup agent to check the first and later
native titles. It checks the application identity, icon, and working directory
on macOS, and the HWND icon and explicit taskbar identity on Windows. Provision
the fixture with LWJGL core/GLFW jars and platform natives, JNA, the pack's
three branding files, and the shipped Java 17. On macOS the packaged helper
can run the fixture with `scripts/fixtures/branding/run_mac_probe.py`.
`verify_game_lifecycle` also exercises the launcher's production game path,
including PID tracking, output, exit status, redaction and parent exit in an
isolated data directory.
