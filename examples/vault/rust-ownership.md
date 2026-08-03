---
id: rust-ownership
title: Rust Ownership
topic: Rusty
tags: [rust, memory]
---

# Rust Ownership {#root}

## Ownership rules {#ownership-rules}

Every value has exactly one owner. When the owner leaves scope, the value is dropped.

supports:: [[rust-ownership#borrowing]]

```quiz
id: ownership-model
type: cloze
prompt: |
  Every value has one {{c1::owner}}.
  When the owner leaves scope, the value is {{c2::dropped}}.
```

## Borrowing {#borrowing}

Borrowing lets code access a value without owning it.

prerequisite:: [[rust-ownership#ownership-rules]]

```quiz
id: owner-scope
type: multiple-choice
mode: single
question: What happens when a value's owner leaves scope?
answers:
  - id: copied
    text: The value is copied
    correct: false
  - id: dropped
    text: The value is dropped
    correct: true
  - id: global
    text: The value becomes global
    correct: false
explanation: Rust automatically calls `drop` when the owner leaves scope.
```

## Code example {#code-example}

Fill the iterator expression.

```quiz
id: vector-loop
type: code-gap
language: rust
prompt: Complete the loop.
code: |
  let values = vec![1, 2, 3];
  for value in {{gap:iterator}} {
      println!("{value}");
  }
gaps:
  iterator:
    answers: [values, values.iter()]
    match:
      trim: true
      normalize_whitespace: true
```
