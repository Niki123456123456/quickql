# Values and Expressions

Every value in QuickQL — whether on the right side of an assignment, inside a function call, or used directly in `FILTER` — is one of the following types.

---

## Field references

A bare identifier refers to a field on the current row. Dots create a path through nested objects.

```
name
user_id
address.city
order.shipping.country
```

Use `$` to refer to the whole current row/value.

```
$
$.name
```

If the path does not exist, the value is `null`.

```ql
SOURCE OPEN('users.json')
MAP id, city = address.city, row = $
```

## Secret references

Prefix a name with `@` to resolve a secret at runtime instead of reading a field
from the current row:

```ql
SOURCE GET({
  src: 'https://api.example.com/items',
  headers: {Authorization: CONCAT('Bearer ', @API_TOKEN)}
})
```

QuickQL first checks the process environment, then checks for a file with the
secret's name below `SECRETS_PATH`. If a secret is unavailable, its value is
`null`. The VS Code extension also reads `.env` from the `.ql` file's workspace
folder whenever a query runs; existing process environment values take
precedence.

---

## String literals

Both single and double quotes are supported. Use a backslash to escape the quote character.

```
'hello world'
"hello world"
'it\'s fine'
"say \"hi\""
```

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'active')
```

---

## Number literals

Integers and decimals. Negative values are prefixed with `-`.

```
42
-7
3.14
-0.001
```

```ql
SOURCE OPEN('products.json')
FILTER EQ(category_id, 5)
```

---

## Boolean literals

Case-insensitive.

```
true
false
TRUE
FALSE
```

```ql
SOURCE OPEN('users.json')
FILTER EQ(verified, true)
```

---

## Inline JSON objects

Curly-brace syntax mirrors JSON objects. Keys can be unquoted identifiers or quoted strings. Values are any QuickQL value expression (including field references and function calls).

```
{key: value}
{name: 'Alice', active: true}
{region: address.country, total: SUM(amount)}
```

```ql
SOURCE OPEN('users.json')
MAP id, meta = {role: role, verified: verified}
```

Keys with special characters must be quoted:

```
{'content-type': 'application/json'}
```

---

## Inline JSON arrays

Square-bracket syntax. Elements are any value expression.

```
[1, 2, 3]
['a', 'b', 'c']
[id, name, email]
```

```ql
SOURCE OPEN('products.json')
MAP id, labels = [category, subcategory]
```

---

## Function calls

Function names are case-insensitive. Arguments are comma-separated value expressions.

```
SUM(amount)
CONCAT(first_name, ' ', last_name)
EQ(status, 'pending')
AND(active, EQ(role, 'admin'))
```

See [functions.md](functions.md) for the full list.

---

## Nesting

Values compose freely. Function arguments can be other function calls, inline objects, or inline arrays.

```ql
SOURCE OPEN('data.json')
MAP *, label = CONCAT(name, ' (', GETDATE(created_at), ')')
```

```ql
SOURCE OPEN('orders.json')
FILTER AND(EQ(status, 'active'), OR(EQ(region, 'EU'), EQ(region, 'US')))
```

---

## Truthiness (for FILTER)

Any value can be used in `FILTER`. The row is kept when the value is truthy:

| Value | Truthy? |
|-------|---------|
| `true` | yes |
| `false` | no |
| non-zero number | yes |
| `0` | no |
| non-empty string | yes |
| `""` | no |
| non-empty array | yes |
| `[]` | no |
| non-empty object | yes |
| `{}` | no |
| `null` (missing field) | no |

---

## Comments

Lines or line tails starting with `--` are ignored.

```ql
SOURCE OPEN('users.json')          -- load all users
FILTER active              -- keep active users only
MAP id, name, email        -- strip sensitive fields
```
