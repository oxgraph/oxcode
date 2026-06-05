; Ruby symbol-extraction query.
(class name: (constant) @name) @definition.class
(module name: (constant) @name) @definition.module
(method name: (identifier) @name) @definition.method
(singleton_method name: (identifier) @name) @definition.method
(call method: (identifier) @name) @reference.call
