[
  "datasource"
  "enum"
  "generator"
  "model"
  "type"
  "view"
] @keyword

(comment) @comment
(developer_comment) @comment

(number) @number
(string) @string

[
  (true)
  (false)
  (null)
] @number

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

(maybe) @punctuation.special

(model_declaration
  (identifier) @type)

(view_declaration
  (identifier) @type)

(enum_declaration
  (identifier) @type)

(type_declaration
  (identifier) @type)

(column_type
  (identifier) @type)

(assignment_expression
  (variable) @property)

(call_expression
  (identifier) @function)

; Attribute names come after the call pattern above so `@default(...)` reads as an attribute
; while the functions inside its arguments keep the function color.
(attribute
  "@" @property)

(attribute
  (identifier) @property)

(attribute
  (call_expression
    (identifier) @property))

(attribute
  (call_expression
    (member_expression
      (identifier) @property)))

(block_attribute_declaration
  "@@" @property)

(block_attribute_declaration
  (identifier) @property)

(block_attribute_declaration
  (call_expression
    (identifier) @property))
