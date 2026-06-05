; Kotlin symbol-extraction query.
(class_declaration (type_identifier) @name) @definition.class
(object_declaration (type_identifier) @name) @definition.class
(function_declaration (simple_identifier) @name) @definition.function
(call_expression (simple_identifier) @name) @reference.call
