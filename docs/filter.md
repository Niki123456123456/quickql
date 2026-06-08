# FILTER

Keeps only rows for which the expression evaluates to a truthy value. Rows where the expression is `false`, `null`, `0`, `""`, `[]`, or `{}` are dropped.

## Syntax

```
FILTER value
```

`value` can be a field reference, a boolean literal, or a function call.

---

## Truthiness rules

| Value | Truthy? |
|-------|---------|
| `true` | yes |
| `false` | no |
| any non-zero number | yes |
| `0` | no |
| non-empty string | yes |
| `""` | no |
| non-empty array | yes |
| `[]` | no |
| non-empty object | yes |
| `{}` | no |
| `null` | no |

---

## Filter by a boolean field

```ql
SOURCE OPEN('users.json')
FILTER active
```

Keeps rows where `active` is truthy.

---

## Filter by equality

Use `EQ(a, b)` to compare values.

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'shipped')
```

---

## Filter with AND / OR

```ql
SOURCE OPEN('orders.json')
FILTER AND(EQ(status, 'pending'), total)
```

Keeps rows where `status` is `'pending'` and `total` is non-zero.

```ql
SOURCE OPEN('orders.json')
FILTER OR(EQ(status, 'pending'), EQ(status, 'processing'))
```

---

## Filter by nested field

```ql
SOURCE OPEN('users.json')
FILTER address.verified
```

---

## Negate with EQ and false

There is no built-in `NOT`. Use `EQ(field, false)` to test for falsy equality:

```ql
SOURCE OPEN('users.json')
FILTER EQ(banned, false)
```

---

## Chain multiple filters

Each `FILTER` step is applied in sequence. They are equivalent to `AND`.

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'shipped')
FILTER total
```

Same as `FILTER AND(EQ(status, 'shipped'), total)`.

---

## Filter after transformation

`FILTER` works on whatever the current row shape is, so you can filter on computed fields added by a preceding `MAP`.

```ql
SOURCE OPEN('orders.json')
MAP *, revenue = SUM(total, tax)
FILTER revenue
```
