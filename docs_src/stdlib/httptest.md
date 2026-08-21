# `std::httptest`

Status: experimental

Fixtures for testing HTTP code. A handler is a function from a request to a response, so `record` calls one in memory; a test that is about the wire builds an `http::Server`, binds port 0, and reads the address back.

