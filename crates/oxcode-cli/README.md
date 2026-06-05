# oxcode

Index source code into a graph and serve it to coding agents. Built on
[oxgraph](https://github.com/oxgraph/oxgraph).

```sh
cargo install oxcode-cli   # installs the `oxcode` binary
oxcode index --path .
oxcode context "How does X work?" --path .
oxcode symbols "auth middleware" --path .
```

Supports Rust, Go, TypeScript/JavaScript, Python, Java, C, C++, C#, PHP, Ruby,
Swift, Kotlin, Scala, Dart, Lua, Luau, Objective-C, Pascal, Svelte, and Vue.

See the [project README](https://github.com/oxgraph/oxcode#readme) for the full
language list, all commands, and the MCP server (`oxcode-mcp`).
