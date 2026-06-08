# SORT_BY

Sorts rows by one or more fields. The default direction is ascending. Null values sort before all other values.

## Syntax

```
SORT_BY key [ASC|DESC] [, key [ASC|DESC] ...]
```

---

## Sort ascending (default)

```ql
SOURCE OPEN('products.json')
SORT_BY name
```

Equivalent to:

```ql
SOURCE OPEN('products.json')
SORT_BY name ASC
```

---

## Sort descending

```ql
SOURCE OPEN('orders.json')
SORT_BY total DESC
```

---

## Sort by multiple keys

Secondary keys are used as tiebreakers when the primary key is equal.

```ql
SOURCE OPEN('products.json')
SORT_BY category ASC, price DESC
```

---

## Sort by nested field

Use dot notation to sort by a field inside a nested object.

```ql
SOURCE OPEN('users.json')
SORT_BY address.country, name
```

---

## Sort numbers

Numbers compare numerically, not lexicographically.

```ql
SOURCE OPEN('invoices.json')
SORT_BY amount DESC
```

---

## Sort strings

Strings compare lexicographically (alphabetical order).

```ql
SOURCE OPEN('users.json')
SORT_BY last_name, first_name
```

---

## Sort dates

Date strings in ISO format (`YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS`) sort correctly as strings because they are zero-padded. Use `GETDATE` in a preceding `MAP` step to normalize datetime strings to date-only before sorting.

```ql
SOURCE OPEN('events.json')
MAP *, date = GETDATE(timestamp)
SORT_BY date DESC
```

---

## Null handling

`null` values sort before all other values in ascending order and after all values in descending order.

---

## Sort is stable

Rows with equal values for all sort keys maintain their original relative order.
