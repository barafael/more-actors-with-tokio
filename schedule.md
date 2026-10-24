Hook: 2 minutes
Title
Arc<Mutex> (mutex game)

Prerequisites: 10 minutes
Futures: timer, button future
Combining Futures: select, loop / select
Spawning: tokio::spawn and its JoinHandle
Channels: mpsc, oneshot
All of it together: watchdog

The Pattern: 12 minutes
essentially this: https://barafael.github.io/posts/more-actors-with-tokio/
Recipe
The event loop as consuming method
Leave the spawning to the user
No actor handles
Unit testing an actor (stepper)
Message enums
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
shoutout to Alice Ryhl (original Blogpost on the matter)
call-to-action to report issues with the pattern against my blog repo.

Bonus Material
