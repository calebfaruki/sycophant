# sa-token-audience

The apiserver's own audience-scoping contract: one service-account token belongs
to one controller and works nowhere else.

- `wrong-audience-rejected` — a token minted for one controller's audience fails
  TokenReview against a different audience, is accepted (and echoed) against its
  own, and is rejected outright as a raw apiserver bearer credential.
