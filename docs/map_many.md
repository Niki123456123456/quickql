# MAP_MANY

Flattens an array field so each element becomes its own row. This is the QuickQL equivalent of SQL `UNNEST` or a lateral join.

## Syntax

```
MAP_MANY field
```

`field` must be the name of a field whose value is an array. Each element of that array replaces the parent row. Non-array values and `null` are silently skipped. Any other type causes an error.

---

## Basic example

Source data `invoices.json`:
```json
[
  {"invoice_id": 1, "lines": [{"sku": "A1", "qty": 2}, {"sku": "B3", "qty": 1}]},
  {"invoice_id": 2, "lines": [{"sku": "C5", "qty": 5}]}
]
```

```ql
SOURCE OPEN('invoices.json')
MAP_MANY lines
```

Output:
```json
[
  {"sku": "A1", "qty": 2},
  {"sku": "B3", "qty": 1},
  {"sku": "C5", "qty": 5}
]
```

Note: parent fields like `invoice_id` are not automatically carried over. Use `MAP` before `MAP_MANY` to keep parent context if needed (see below).

---

## Keeping parent fields

To carry parent data into each flattened row, use `MAP` first to embed the parent key inside the array elements, or restructure with a computed field.

Alternatively, access via a field that was already inside the array items:

```json
[
  {"invoice_id": 1, "lines": [{"invoice_id": 1, "sku": "A1"}, {"invoice_id": 1, "sku": "B3"}]}
]
```

```ql
SOURCE OPEN('invoices.json')
MAP_MANY lines
MAP invoice_id, sku
```

---

## Pipeline position

`MAP_MANY` can appear anywhere in the pipeline after `SOURCE`. A common pattern:

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'shipped')      -- filter parent rows first
MAP_MANY items                     -- then flatten child array
MAP product_id, quantity, price    -- then shape child rows
GROUP_BY product_id MAP qty = SUM(quantity)
```

---

## Nested flattening

Call `MAP_MANY` twice to flatten two levels of nesting.

```ql
SOURCE OPEN('reports.json')
MAP_MANY sections
MAP_MANY rows
```

---

## Dot-path field

`MAP_MANY` accepts a dot-path to reach a nested array.

```ql
SOURCE OPEN('data.json')
MAP_MANY result.items
```
