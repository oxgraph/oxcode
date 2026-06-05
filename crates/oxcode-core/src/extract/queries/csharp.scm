; C# symbol-extraction query.
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(struct_declaration name: (identifier) @name) @definition.struct
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.class
(namespace_declaration name: (identifier) @name) @definition.namespace
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method
(invocation_expression function: (identifier) @name) @reference.call
(invocation_expression
  function: (member_access_expression name: (identifier) @name)) @reference.call
