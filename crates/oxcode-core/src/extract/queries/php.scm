; PHP symbol-extraction query.
(function_definition name: (name) @name) @definition.function
(class_declaration name: (name) @name) @definition.class
(interface_declaration name: (name) @name) @definition.interface
(trait_declaration name: (name) @name) @definition.trait
(enum_declaration name: (name) @name) @definition.enum
(method_declaration name: (name) @name) @definition.method
(function_call_expression function: (name) @name) @reference.call
(member_call_expression name: (name) @name) @reference.call
