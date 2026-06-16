//! A stateful service whose operations can *fail without dying*.
//!
//! Teaches: an `ask` whose answer is itself a `Result`. A handler that returns
//! `Err` is just a normal answer - the error travels back to the asker and the
//! actor keeps running. Contrast this with `#[handler(die_on_err)]`, which would
//! forward the same `Err` *and then kill the actor*.
//!
//! Run with: `cargo run -p factories --example bank_account`

use factories::prelude::*;

/// A recoverable, domain-level error. It is an ordinary value carried back as
/// (part of) the answer - nothing about returning it touches the actor's life.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Overdraft {
    requested: u64,
    available: u64,
}

/// The account owns its balance; every handler runs with exclusive/shared
/// access to it, so no locking discipline leaks into the operations.
#[derive(Actor)]
struct BankAccount {
    balance: u64,
}

#[factories::messages]
impl BankAccount {
    /// `&mut` mutator. The answer is the new balance, so callers can `ask`.
    #[handler]
    fn deposit(&mut self, amount: u64) -> u64 {
        self.balance += amount;
        self.balance
    }

    /// The interesting one: a fallible operation. The handler's return type is
    /// `Result<u64, Overdraft>`, and *that whole Result is the message answer*.
    ///
    /// There is no `die_on_err` here, so the `Err` is treated like any other
    /// value: it is handed to the asker and control returns to the run loop.
    /// The actor is none the wiser and processes the next message normally.
    ///
    /// Had we written `#[handler(die_on_err)]` instead, the asker would receive
    /// the very same `Err(Overdraft { .. })`, but the error would *also* be fed
    /// to the actor's death - every later message would hit a dead mailbox.
    #[handler]
    fn withdraw(&mut self, amount: u64) -> Result<u64, Overdraft> {
        if amount > self.balance {
            // Recoverable: report what went wrong and leave the balance intact.
            return Err(Overdraft {
                requested: amount,
                available: self.balance,
            });
        }
        self.balance -= amount;
        Ok(self.balance)
    }

    /// `&self` reader: shared access, no mutation. Pure query of current state.
    #[handler]
    async fn balance(&self) -> u64 {
        self.balance
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Spawn the account onto the current tokio runtime.
    let account = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), BankAccount { balance: 0 })
        .await?;

    // A deposit is a plain ask: the answer is the new balance.
    let balance = account.deposit(100).await?;
    println!("deposited 100 -> balance {balance}");

    // A withdrawal we can afford: the answer's inner Result is `Ok`.
    // The outer `?` unwraps the send/transport result; the inner Result is the
    // handler's own domain answer.
    match account.withdraw(40).await? {
        Ok(remaining) => println!("withdrew 40 -> balance {remaining}"),
        Err(over) => println!("withdraw 40 refused: {over:?}"),
    }

    // An overdrawing withdrawal: the handler returns `Err`. The send still
    // succeeds - the `Err` is the answer - so `?` is happy and we see the
    // domain error come back, while the actor remains perfectly alive.
    match account.withdraw(1_000).await? {
        Ok(remaining) => println!("withdrew 1000 -> balance {remaining}"),
        Err(over) => println!(
            "withdraw 1000 refused: wanted {} but only {} available (actor still alive)",
            over.requested, over.available
        ),
    }

    // Proof the actor survived the `Err`: it answers another query, and the
    // balance is exactly what it was before the failed withdrawal.
    let still_there = account.balance().await?;
    println!("balance after the refused withdrawal -> {still_there}");
    assert_eq!(still_there, 60, "the failed withdraw must not have moved money");

    Ok(())
}
