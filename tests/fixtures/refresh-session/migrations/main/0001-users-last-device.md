# users last_device

## Target

- provider: main
- seq: 0001

## Up

### s1

- sql: ALTER TABLE users ADD COLUMN IF NOT EXISTS last_device text

## Verify

(none)

## Data

(none)
