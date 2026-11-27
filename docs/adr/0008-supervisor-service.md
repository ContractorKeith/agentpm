# One local supervisor is the sole writer of run state

The supervisor service is the only writer of `state.sqlite`. Clients talk to it over a local socket, one request per connection. Client handling is synchronous with 2-second read and write deadlines, and frames are read in bounded chunks; child execution runs on separate threads. The service sets umask 077 so every file it creates, including SQLite journals, is private.

A slow client can delay others by up to the deadline; for a single-user local tool this is accepted over a concurrent request path.
