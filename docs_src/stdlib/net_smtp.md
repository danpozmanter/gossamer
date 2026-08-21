# `std::net::smtp`

Status: experimental

Sends one message per call, so an application can mail a password reset, an address verification, a magic link, or a security notice. A pool, a queue, retries, and bounce handling are application policy and belong in a package built on these. Port 465 speaks TLS from the first byte; any other port starts in the clear and upgrades through STARTTLS when the server offers it, and credentials are refused rather than sent to a server offering no encryption.

