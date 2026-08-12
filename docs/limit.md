# LIMIT

Keeps at most the specified number of rows from the current pipeline result.

## Syntax

```ql
LIMIT count
```

`count` must be a non-negative integer. `LIMIT 0` returns no rows. Because statements run from top to bottom, place `LIMIT` after `SORT_BY` to keep the first rows in sorted order.

```ql
SOURCE OPEN('products.json')
SORT_BY price DESC
LIMIT 1024
```
