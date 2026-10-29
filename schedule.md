Hook: 2 minutes
Title
Prelude: communication implies trust; trust requires responsibility; responsibility means ownership
Arc<Mutex> (mutex game)

Prerequisites: 10 minutes
Futures: timer, button future
Combining Futures: select, loop / select
Spawning: tokio::spawn the loop-select; its JoinHandle never resolves
Channels: mpsc, oneshot
All of it together: watchdog
Loop-select pitfalls (links the loop-select post)

The Pattern: 12 minutes
essentially this: https://barafael.github.io/posts/more-actors-with-tokio/
Recipe
Message enums
The event loop as consuming method
Leave the spawning to the user
No actor handles
Unit testing an actor (stepper)
Call and response (mpsc + oneshot)
There is no Rust actor framework, and it's called tokio
Don't make a Heartbeat actor
The only thing better than an actor: no actor

Architecture Consequences: 12 minutes
Cancellation Cascade: FuturesUnordered, pipeline, explicit shutdown
Panic Safety
Deadlock Possibility (DAG to avoid): cycle game
Other Channels: broadcast, watch
In practice: budget chat, speedd

Closing: 4 minutes
Mention oOOP (Alan Kay connection)
Ownership is Responsibility
shoutout to Alice Ryhl (original Blogpost on the matter)
call-to-action to report issues with the pattern against my blog repo.

Bonus Material
One actor, any wire: Stream/Sink/AsyncRead adapters (TCP/JSON, TCP/protobuf, WebSocket/JSON, WebTransport/postcard)
