; Scala symbol-extraction query.
(class_definition name: (identifier) @name) @definition.class
(object_definition name: (identifier) @name) @definition.class
(trait_definition name: (identifier) @name) @definition.trait
(function_definition name: (identifier) @name) @definition.function
(call_expression function: (identifier) @name) @reference.call
