# GROUP_BY

Groups rows by one or more key fields and produces one output row per group. An optional `MAP` clause computes aggregate values for each group.

## Syntax

```
GROUP_BY key [, key ...] [MAP item [, item ...]]
```

Use `*` as the key to group all rows together into a single group.

---

## How grouping works

After grouping, each group is represented as an object whose fields hold **arrays** of the values from all rows in that group. Aggregate functions like `SUM`, `COUNT`, `MINDATE`, and `MAXDATE` operate on those arrays.

Given rows:
```json
[
  {"region": "EU", "amount": 100},
  {"region": "EU", "amount": 200},
  {"region": "US", "amount": 150}
]
```

After `GROUP_BY region`, each group's aggregate value is:
```
EU → {"region": ["EU","EU"], "amount": [100, 200]}
US → {"region": ["US"],      "amount": [150]}
```

The `MAP` clause computes the output from those arrays.

---

## Basic aggregation

```ql
SOURCE OPEN('sales.json')
GROUP_BY region MAP revenue = SUM(amount), orders = COUNT(amount)
```

Output:
```json
[
  {"region": "EU", "revenue": 300, "orders": 2},
  {"region": "US", "revenue": 150, "orders": 1}
]
```

---

## Group by multiple keys

```ql
SOURCE OPEN('sales.json')
GROUP_BY region, category MAP revenue = SUM(amount)
```

---

## Group all rows (no key)

Use `*` to collapse all rows into one group — useful for global totals.

```ql
SOURCE OPEN('orders.json')
GROUP_BY * MAP total = SUM(amount), count = COUNT(amount)
```

Output: a single row with the totals.

---

## Date aggregation

```ql
SOURCE OPEN('events.json')
GROUP_BY user_id MAP first = MINDATE(created_at), last = MAXDATE(created_at)
```

---

## No MAP clause

Without `MAP`, `GROUP_BY` still groups and deduplicates by the key, outputting one row per unique key combination (only key columns in the result).

```ql
SOURCE OPEN('orders.json')
GROUP_BY customer_id
```

Produces one row per unique `customer_id`.

---

## Combine with other steps

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'completed')
GROUP_BY customer_id MAP total = SUM(amount), last_order = MAXDATE(completed_at)
SORT_BY total DESC
```

---

## Group key order is preserved

Groups appear in the order the first row with that key was encountered in the input.
