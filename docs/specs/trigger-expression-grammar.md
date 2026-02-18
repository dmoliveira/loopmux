# Trigger Expression Grammar (`--trigger-expr`)

## Goal
Add a composable trigger mode for lean runs that supports boolean logic without breaking existing `--trigger` behavior.

## Compatibility Contract
- `--trigger` keeps current semantics: a single regex pattern matched against the captured source text.
- `--trigger-expr` introduces expression mode and is mutually exclusive with `--trigger`.
- Existing scripts that only use `--trigger` continue to behave exactly as before.

## CLI Contract
- New flag: `--trigger-expr <expr>`.
- Validation rules:
  - Require `--prompt` (same as `--trigger`).
  - Conflict with `--config`.
  - Conflict with `--trigger`.
- `--trigger-exact-line` applies only to `--trigger` (not `--trigger-expr`) in v1.

## Expression Syntax
- Operators:
  - `&&` = logical AND
  - `||` = logical OR
- Grouping:
  - `(` and `)`
- Terms:
  - Raw regex atoms (same matching engine as current `--trigger`).

Examples:
- `"DONE || READY"`
- `"(ERROR || FAIL) && RETRY"`
- `"<CONTINUE-LOOP> && (LGTM || APPROVED)"`

## Precedence and Associativity
- Precedence (high to low):
  1. Parenthesized group
  2. `&&`
  3. `||`
- Associativity:
  - Left-associative for both `&&` and `||`.

Equivalent parse example:
- `A || B && C` parses as `A || (B && C)`.

## Evaluation Semantics
- Each term is compiled as regex and evaluated against the same captured source text window.
- Short-circuit rules:
  - `A || B`: if `A` is true, do not evaluate `B`.
  - `A && B`: if `A` is false, do not evaluate `B`.
- Evaluation output is a single boolean used by the existing trigger pipeline (edge, confirm, recheck).

## Error Handling
Parser errors must include byte/character position and a concise reason.

Required diagnostics:
- Unexpected token.
- Missing right parenthesis.
- Empty term around operator.
- Trailing operator.
- Invalid regex term.

Example message shape:
- `invalid trigger expression at pos 7: expected term after '&&'`

## Implementation Notes (for `bd-25x` and `bd-345`)
- Tokenizer emits: `Term(String)`, `And`, `Or`, `LParen`, `RParen`.
- Parser strategy: precedence-climbing or shunting-yard to AST.
- AST nodes:
  - `Term(String)`
  - `And(Box<Expr>, Box<Expr>)`
  - `Or(Box<Expr>, Box<Expr>)`
- Regex compile cache should be per expression parse to avoid recompiling every poll cycle.

## Acceptance Criteria
- Grammar/compatibility behavior is explicit and stable.
- `--trigger` remains backward-compatible.
- `--trigger-expr` has deterministic precedence and grouping rules.
- Error messages are position-aware and actionable.
