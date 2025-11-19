---
tags:
  - search
---

# ExtendedSearch

{{since('nightly')}}

The `ExtendedSearch` struct contains the following fields:
* `pattern` - allows below options
    * `{ CaseSensitiveString = '' }`
    * `{ CaseInSensitiveString = '' }`
    * `{ Regex = '' }`
    * `"CurrentSelectionOrEmptyString"`
* `activate_match` - string parameter indicating the match to activate. Valid values are `AfterCursor`, `BeforeCursor`, `First`.
