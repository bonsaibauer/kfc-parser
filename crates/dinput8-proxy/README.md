# dinput8-proxy

This crate provides a proxy for the `dinput8.dll` library.
It is the entry point for the mod loader patcher and runtime.

## ShroudForge native DLL chain

ShroudForge owns discovery, ordered loading, and `dinput8-chain.ini` parsing in
its DLL Manager. This proxy does not load a second DLL chain. After loading a
native mod, the manager can call the exported
`BOOL WINAPI ShroudforgeSetDinput8Provider(HMODULE module)` function. The first
module exporting its own `DirectInput8Create` becomes the provider. Selecting
the same provider again succeeds. Selecting a different provider, this proxy,
or the original System32 implementation returns `FALSE`.

The manager keeps selected modules loaded until process exit. Only
`DirectInput8Create` is routed to the provider. The other five Windows exports
continue to use System32. A provider that calls back into this proxy falls back
to the original Windows function, preventing a recursive chain. Provider
selection and execution happen outside `DllMain`. The existing ShroudForge
and EML startup selection is unchanged.

## Development on Steam (Linux Only)

If you want to have a console for debugging purposes,
change the Launch Options of the game in Steam to:

```bash
EML_CONSOLE=1 %command%
```

## Environment Variables

- `EML_CONSOLE`: Enable or disable the console. Default is `false`.
- `EML_LOG_FILE_FILTER`: Filter for file logging. [format](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives)
- `EML_LOG_FILE_ENABLED`: Enable or disable file logging. Default is `true`.
- `EML_LOG_FILE_PATH`: Path to the log file. Default is `./logs`
- `EML_LOG_FILE_MAX`: Maximum amount of log files to keep. Default is `128`.
- `EML_LOG_STDOUT_FILTER`: Filter for stdout logging. [format](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives)
- `EML_LOG_STDOUT_ENABLED`: Enable or disable stdout logging. Default is `true
