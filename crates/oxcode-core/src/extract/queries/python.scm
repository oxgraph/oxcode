; Python symbol-extraction query for the generic query-driven extractor.
; Capture vocabulary is fixed by oxcode (see profile.rs): a `@definition.*`
; marks a symbol's anchor node, `@name` its identifier, `@reference.*` a call,
; and `@reference.qualifier` a call receiver.

(function_definition
  name: (identifier) @name) @definition.function

(class_definition
  name: (identifier) @name) @definition.class

; Bare call: `helper()`
(call
  function: (identifier) @name) @reference.call

; Attribute call: `receiver.method()`
(call
  function: (attribute
    object: (identifier) @reference.qualifier
    attribute: (identifier) @name)) @reference.call
