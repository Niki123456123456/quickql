# Functions

Built-in functions are available in any expression position — inside `MAP`, `FILTER`, `SOURCE`, and the `MAP` clause of `GROUP_BY`. Function names are case-insensitive.

---

## Aggregate functions

These functions are primarily useful inside `GROUP_BY MAP` where a field holds an array of values collected from a group. They also accept plain scalar or array values directly.

### SUM

Sums numeric values. Non-numeric strings are parsed as numbers; values that cannot be parsed contribute `0`. Returns an integer when the result has no fractional part.

```ql
SOURCE OPEN('sales.json')
GROUP_BY region MAP total = SUM(amount)
```

```ql
-- sum two fields per row
SOURCE OPEN('orders.json')
MAP *, total = SUM(price, shipping_fee)
```

---

### COUNT

Counts the number of values. When given a grouped array the result is the number of rows in the group.

```ql
SOURCE OPEN('orders.json')
GROUP_BY status MAP n = COUNT(id)
```

---

### ARRAY

Collects arguments into a flat array. Existing arrays in the arguments are flattened.

```ql
SOURCE OPEN('orders.json')
GROUP_BY customer_id MAP order_ids = ARRAY(id)
```

---

### ASSIGN

Shallow-merges object fields like JavaScript `Object.assign`. Later objects overwrite earlier fields. Returns `null` when any argument is not an object.

```ql
MAP merged = ASSIGN({name: 'a', nested: {left: 1}}, {value: 2, nested: {right: 2}})
-- {name: 'a', value: 2, nested: {right: 2}}
```

---

### MINDATE / MAXDATE

Returns the earliest or latest date string from a set. Understands ISO date formats: `YYYY-MM-DD`, `DD-MM-YYYY`, `DD.MM.YYYY`, and ISO datetimes (`YYYY-MM-DDTHH:MM:SS...`). Returns `null` if no parseable date is found.

```ql
SOURCE OPEN('events.json')
GROUP_BY user_id MAP first_seen = MINDATE(created_at), last_seen = MAXDATE(created_at)
```

---

## String functions

### CONCAT

Concatenates all arguments into a single string. Arrays in arguments are flattened. Non-string values are converted to their JSON representation; `null` becomes an empty string.

```ql
SOURCE OPEN('users.json')
MAP *, full_name = CONCAT(first_name, ' ', last_name)
```

---

### BASE64

Base64-encodes the string representation of its argument.

```ql
SOURCE OPEN('api_keys.json')
MAP id, encoded = BASE64(secret)
```

---

### SPLIT

Splits a string into equal parts with the given maximum part length. Returns `null` when the first argument is not a string or the length is not a positive integer.

```ql
MAP parts = SPLIT('abcdefghij', 4)
-- ['abcd', 'efg', 'hij']
```

---

### PARSE

Parses a JSON string and returns the resulting JSON value. Returns `null` when the input is not a string or is not valid JSON.

```ql
MAP parsed = PARSE('{"name":"a","value":1}')
-- {name: 'a', value: 1}
```

---

### GETDATE

Extracts the date portion (`YYYY-MM-DD`) from an ISO datetime string. Returns `null` if the input is not a datetime string.

```ql
SOURCE OPEN('events.json')
MAP *, date = GETDATE(created_at)
```

Input: `"2024-03-15T08:30:00Z"` → Output: `"2024-03-15"`

---

### ISODATE

Normalizes parseable date strings to ISO date format (`YYYY-MM-DD`). Returns `null` when the input is not a valid date string.

```ql
SOURCE OPEN('events.json')
MAP *, date = ISODATE(raw_date)
```

Input: `"24.03.2026"` → Output: `"2026-03-24"`

---

## Logic functions

### EQ

Returns `true` if both arguments are equal (strict equality).

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'shipped')
```

---

### AND

Returns `true` if all arguments are truthy.

```ql
SOURCE OPEN('orders.json')
FILTER AND(EQ(status, 'pending'), total)
```

---

### OR

Returns `true` if at least one argument is truthy.

```ql
SOURCE OPEN('orders.json')
FILTER OR(EQ(status, 'pending'), EQ(status, 'processing'))
```

---

### LEN

Returns the number of arguments (not the length of a string or array). Primarily useful for counting fixed-length argument lists.

```ql
SOURCE OPEN('data.json')
MAP n = LEN(a, b, c)   -- always 3
```

---

### RANGE

Returns an inclusive array of integers from the first argument to the second argument.

```ql
MAP values = RANGE(0, 2)      -- [0, 1, 2]
MAP values = RANGE(-3, -1)    -- [-3, -2, -1]
MAP values = RANGE(2, 0)      -- [2, 1, 0]
```

---

### AT

Returns the item in the first argument array at the zero-based integer index from the second argument. Returns `null` when the first argument is not an array, the index is invalid, or the index is out of range.

```ql
MAP first = AT(items, 0)
MAP second = AT(['a', 'b', 'c'], 1)  -- 'b'
```

---

### ZIPROWS

Converts an object whose fields are arrays into an array of row objects by matching values at the same index. Returns `null` when the input is not an object, any field is not an array, or the arrays have different lengths.

```ql
MAP rows = ZIPROWS({name: ['a', 'b', 'c'], value: [1, 2, 3]})
-- [
--   {name: 'a', value: 1},
--   {name: 'b', value: 2},
--   {name: 'c', value: 3}
-- ]
```

---

## Data-loading functions

These functions resolve a source at query time, returning the loaded data as a value. Use them inside `SOURCE` or inside a `MAP` assignment.

### GET / OPEN

Perform an HTTP GET request (or open a local file) and return the parsed JSON.

```ql
SOURCE GET('https://api.example.com/users')
```

With headers and paging:

```ql
SOURCE GET({
  src: 'https://api.example.com/items',
  headers: {Authorization: 'Bearer token'},
  paging: {
    type: 'cursor',
    in:   {location: 'query', path: 'cursor'},
    from: {location: 'body',  path: 'meta.next_cursor'}
  }
})
```

---

### POST

Perform an HTTP POST and return the parsed JSON response.

```ql
SOURCE POST({
  src: 'https://api.example.com/search',
  headers: {Content-Type: 'application/json'},
  body: {filter: 'active', page: 1}
})
```

---

### PUT

Perform an HTTP PUT and return the parsed JSON response.

```ql
SOURCE PUT({
  src: 'https://api.example.com/items/1',
  body: {status: 'archived'}
})
```
