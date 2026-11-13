# Personal builds

All TradeCraft application changes must be included in both the personal and community executables. Keep their provider settings in separate files; retain the personal-only VibesTrader credential shortcut in the personal build. After each change, rebuild both executables and verify both updates succeeded. Never treat a pending executable as an updated taskbar pin.

When building or updating the user's personal TradeCraft executable, run `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-personal.ps1`. The taskbar pin targets `target/personal/tradecraft.exe`; the script updates that stable path after a successful release build with `vibestrader-credential`. Community builds must not replace the personal executable. If TradeCraft is running and the executable cannot be replaced, report that the update needs the app to close; do not silently leave the pin on an old build.
