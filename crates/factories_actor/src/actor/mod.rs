/// The heart of the actor system. Defines a struct as being an actor.
unsafe trait Actor {
    /// The core of the actor that contains the state.
    type ControlBlock;
}


 