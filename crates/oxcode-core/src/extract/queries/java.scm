; Java symbol-extraction query for the generic query-driven extractor.

(class_declaration
  name: (identifier) @name) @definition.class

(interface_declaration
  name: (identifier) @name) @definition.interface

(enum_declaration
  name: (identifier) @name) @definition.enum

(record_declaration
  name: (identifier) @name) @definition.class

(method_declaration
  name: (identifier) @name) @definition.method

(constructor_declaration
  name: (identifier) @name) @definition.method

(field_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @definition.field

(method_invocation
  name: (identifier) @name) @reference.call

(object_creation_expression
  type: (type_identifier) @name) @reference.call
