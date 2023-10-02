# openapi-deref

This is a crate that implements dereferencing open api references from the openapiv3 crate.

This currently uses a fork of that crate which can be found [here](https://github.com/krlohnes/openapiv3/tree/flattening). This essentially add a dereferenced type to the `ReferenceOr` enum so we can have both the `$ref` location and the actual items in that case.
