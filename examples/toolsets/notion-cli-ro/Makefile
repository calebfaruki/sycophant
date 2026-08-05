.PHONY: notion-search notion-page-get notion-database-query \
        notion-block-children-list notion-block-get notion-comment-list \
        notion-users-list notion-user-get notion-whoami

# All recipes reference args via `"$$VAR"`. Make's `$$` escapes to a literal
# `$`, so the shell receives `"$VAR"` — variable expansion in double quotes
# is a single argv token regardless of contents (no re-parsing, no word
# splitting). Never use `$(VAR)` (make-side expansion) for LLM-derived values.

notion-search:
	@jq -n --arg q "$$QUERY" '{query: $$q}' | ntn api v1/search -X POST -d @-

notion-page-get:
	@ntn pages get "$$PAGE_ID"

notion-database-query:
	@ntn api "v1/databases/$$DATABASE_ID/query" -X POST -d "$${FILTER:-{}}"

notion-block-children-list:
	@ntn api "v1/blocks/$$BLOCK_ID/children"

notion-block-get:
	@ntn api "v1/blocks/$$BLOCK_ID"

notion-comment-list:
	@ntn api "v1/comments?block_id=$$BLOCK_ID"

notion-users-list:
	@ntn api v1/users

notion-user-get:
	@ntn api "v1/users/$$USER_ID"

notion-whoami:
	@ntn api v1/users/me
