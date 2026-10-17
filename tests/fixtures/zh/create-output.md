## Route
- method: POST
- path: /catalog/v1/{slug}/products
## Access
- public: true
- public-writes: true
- tenant: path.slug
- challenge: { type: none, when: always }
## Pipelines
### -auth
- removed: true
## Limits
(none)
## Request
｜ field ｜ type ｜ rules ｜ required ｜
｜ --- ｜ --- ｜ --- ｜ --- ｜
｜ body.sku ｜ string ｜ maxLength: 40 ｜ yes ｜
｜ body.name ｜ string ｜ max_length: 120 ｜ yes ｜
## Response
- status: 201
## Transform
- in: {sku: req.body.sku, name: req.body.name}
- out: result
## Steps
### s1
- op: method
- target: product.create
- args: transform.in
- out: result
### s2
- op: respond
- value: transform.out
- status: 201
## Errors
(none)
