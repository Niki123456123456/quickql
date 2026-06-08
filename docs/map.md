# MAP

Transforms each row by selecting, renaming, or computing columns. Rows not mentioned in the mapping are dropped unless `*` is included.

## Syntax

```
MAP item [, item ...]
```

An `item` is one of:

| Form | Description |
|------|-------------|
| `field` | Include the field unchanged |
| `alias = value` | Assign the result of `value` to `alias` |
| `*` | Include all fields from the current row |

---

## Select columns

Keep only specific fields.

```ql
SOURCE OPEN('users.json')
MAP id, name, email
```

Input row:
```json
{"id": 1, "name": "Alice", "email": "alice@example.com", "password_hash": "..."}
```

Output row:
```json
{"id": 1, "name": "Alice", "email": "alice@example.com"}
```

---

## Rename a field

```ql
SOURCE OPEN('users.json')
MAP id, full_name = name, contact = email
```

Output:
```json
{"id": 1, "full_name": "Alice", "contact": "alice@example.com"}
```

---

## Compute a new field

Use a function or expression on the right side of `=`.

```ql
SOURCE OPEN('orders.json')
MAP id, total, vat = SUM(total, tax), date = GETDATE(created_at)
```

---

## Keep all fields and add new ones

Use `*` to spread existing fields, then add computed columns.

```ql
SOURCE OPEN('orders.json')
MAP *, total_with_tax = SUM(total, tax)
```

---

## Nest into a sub-object using dot notation

The alias supports dot notation to write values into nested objects.

```ql
SOURCE OPEN('users.json')
MAP id, address.city = city, address.country = country
```

Output:
```json
{"id": 1, "address": {"city": "Berlin", "country": "DE"}}
```

---

## Build an inline object or array

```ql
SOURCE OPEN('users.json')
MAP id, meta = {role: role, active: active}
```

```ql
SOURCE OPEN('products.json')
MAP id, tags = [category, subcategory]
```

---

## Read from nested fields

Use dot notation on the right side to read from nested paths.

```ql
SOURCE OPEN('users.json')
MAP id, city = address.city, country = address.country
```

---

## MAP is stateless per row

Every `MAP` step creates a new object from scratch for each row. Fields not listed (and not covered by `*`) are gone.
