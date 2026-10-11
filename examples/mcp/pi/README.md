# pi coding agent — not supported (no native MCP)

Checked on 2026-10-11 with `pi` 0.85.0 (`pi --help`): pi has no MCP client
built in (no option or setting to declare an MCP server); tools come from its
built-ins and from extensions. An MCP bridge would be a third-party
extension (`pi install <source>`): none was installed or tested.

So no LightVM configuration is published for pi (ADR-0021: example
configurations only for clients actually tested). If an MCP extension is
adopted later, it must launch `lightvm-mcp` over stdio with no argument (the
pairing file is read from the app's location or from
`LIGHTVM_PAIRING_FILE`), and the run must be recorded in a verification file
before an example is added here.
