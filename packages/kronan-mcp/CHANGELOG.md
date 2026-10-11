# Changelog

## 0.3.1

- A money tool (`reserve_delivery_slot`, `reserve_pickup_slot`,
  `complete_checkout`, `add_checkout_to_order`) reads the saved token once,
  before its checks, and uses it for the checks and the order call. A token
  that cannot be read now refuses with the token store's own message, and
  nothing is recorded or sent. Before, a token that became unreadable after
  the checks recorded an unknown outcome for an order call that was never
  sent. This is an intended difference from the TypeScript package, which
  reads the token for each request.

## 0.3.0

- The native binary is now built from Rust. Tools, inputs, results, order
  confirmations and the order-attempt record, CLI commands and messages are
  unchanged.
- The access token is kept in the encrypted file store, with its key in a
  separate private file: on macOS in `~/Library/Application Support/family-mcp`,
  which is excluded from Time Machine; on Linux in `~/.config/kronan-mcp`, with
  the key in `~/.local/share/family-mcp/keys`. `kronan-mcp auth status` shows
  how the token is saved.
- Every start checks the store's location and permissions. A refusal names the
  path and, for most problems, the command that fixes it.
- A token saved by 0.2.0 keeps working from its plaintext file. Run
  `kronan-mcp auth migrate` once to move it into the encrypted store and remove
  the file. Order-attempt records stay at
  `<token file>.order-attempts.json` and keep blocking as before.
- Tokens saved with earlier test builds that kept the store key in the macOS
  Keychain are not migrated: remove the files the refusal names, then run
  `kronan-mcp auth set` again.
- When `auth set` is ended by SIGINT or SIGTERM while it waits at the token
  prompt, it restores the terminal and exits with status 130 or 143. Before,
  the process was killed by the signal. A failure without a reviewed message
  now prints `Krónan MCP failed.` instead of the underlying error text.

## 0.2.0

- Adds shopping-note writes: `add_shopping_note_lines`,
  `change_shopping_note_line`, `toggle_shopping_note_line_complete`,
  `delete_shopping_note_line`, and `clear_shopping_note` (requires
  `confirm: true`).
- Adds basket tools: `preview_checkout_lines` (read-only validation) and
  `set_checkout_lines`, which requires an explicit `replace`.
- Adds order tools that can authorize a charge on the saved card:
  `reserve_delivery_slot`, `reserve_pickup_slot`, `complete_checkout`, and
  `add_checkout_to_order`. Each requires `confirm: true`, `expectedTotal`, and
  `expectedCheckoutToken`, re-reads the checkout, and refuses without calling
  Krónan when the checkout is empty or changed. `add_checkout_to_order` also
  requires `expectedOrderToken` and checks the active order.
- Order requests are sent once and never retried. Once sent, any failure,
  4xx and 429 included, returns `outcome: "unknown"`, not a failure; the result
  tells the agent to check `get_active_order` and `list_orders`, ask the user,
  and not retry.
- Allows one attempt per approval. Order calls are recorded in a private
  `<token file>.order-attempts.json` under a cross-process lock, as
  `submitting` before the request and `accepted` or `unknown` after it. An
  unresolved attempt blocks all four order tools for that checkout; an accepted
  one blocks repeating it for the same lines and total, while an accepted
  reserve still allows `complete_checkout`. Only the new
  `kronan-mcp orders clear-attempts` CLI command clears the record, after a y/N
  confirmation.
- `expectedTotal` is documented as a checkout consistency check, not a cap:
  fees and the selected slot can make `authorizedAmount` higher, so agents must
  show the checkout fee fields and get approval of that uncertainty.
- Adds placed-order changes: `delete_order_lines` and
  `lower_order_line_quantities` (both require `confirm: true`) and
  `toggle_order_line_substitution`. Any failure after sending one of them
  means the change may have been applied; read `get_order` first.
- Exposes 41 tools. Annotations mark every removal, checkout replacement, and
  order call as destructive and not idempotent.
- Extends the OpenAPI drift check to every new request body and response.
- The write endpoints are not yet verified against a live Krónan account; the
  order in which reserve and complete combine is unverified.

## 0.1.0

- First release of the unofficial Krónan MCP server.
- Adds product search, lookup, category, tag, sale, and favorite-product tools.
- Adds order, purchase-history, shopping-note, product-list, recipe, address,
  delivery and pickup slot availability, and checkout read tools.
- Exposes 27 read tools; no checkout, order, note, list, favorite, or slot
  mutations are available. `get_checkout` and `get_shopping_note` are annotated
  as not read-only because Krónan creates an empty resource on first read.
- Adds private personal-token authentication through `KRONAN_TOKEN_FILE`
  and `kronan-mcp auth set`; imported token source files must be private.
- Vendors Krónan's OpenAPI document and checks the Zod schemas against
  openapi-typescript output at typecheck time.
- Every read endpoint except the barcode lookup was verified once against a
  live account on 2026-09-15; responses matched the vendored schema.
