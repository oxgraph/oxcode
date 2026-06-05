; Lua/Luau symbol-extraction query.
(function_declaration name: (identifier) @name) @definition.function
(function_declaration
  name: (dot_index_expression field: (identifier) @name)) @definition.function
(function_call name: (identifier) @name) @reference.call
