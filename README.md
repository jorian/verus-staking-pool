# Verus staking pool

This software allows anyone to run a non-custodial staking pool, making use of VerusIDs.

[Install](INSTALL.md)

# The staking pool explained

Ever since the creation of Bitcoin and its Proof-of-Work mining protocol, small miners have been pooling their computational power in order to lower the variance in mining income. The idea is simple: more miners = more blocks. If you bundle a couple of small miners together and divide the mined income proportional to how much work every miner has put in to mine that block, you get a steadier stream of income.

Verus uses a hybrid mechanism: 50% Proof-of-Work (PoW) and 50% Proof-of-Stake (PoS). PoS is a mechanism where the size of your VRSC stake proportional to all VRSC in the network determines your chance of generating the next block. In Verus' case this means: more VRSC = more blocks. 

To pool your VRSC similar to how computational power is pooled with PoW is hard: The essence of Proof-of-Stake is that the funds of which you hold the private key are the miners. You can't point those funds somewhere to stake together, because in order to stake, a native wallet (with the private key that holds your funds) needs to participate in the network to get a chance of staking a block. If you still want to stake in a pool, you'd have to place a lot of trust in a centralized staking pool, by sending them your funds and trust that they will send back your funds when you want to.  

Currently, there is 1 such staking pool for Verus, called Dudezmobi staking. It is run by a Verus community member and requires users that want to pool their stake to send over their funds to this community member. In other words, a custodial staking pool.  

Synergo pool is a new staking pool for the Verus Platform that solves the problem of having to give away control of your funds in order to stake in a pool.
Synergo pool uses the following VerusID primitives to make this possible:

**VerusID**  
[Verus Vault](https://docs.verus.io/verusid/#verus-vault) allows VerusIDs to be locked. In order to send away any funds, the locked VerusID must be unlocked first, except for when a stake is found. A locked VerusID can stake while being locked. This is useful for staking in a pool: stake with many people together, while assuring that nobody can run off with your funds, because you locked the funds in the identity.

**Revoke and Recover**  
The Staking pool uses several precautions to ensure no private keys are leaked. In the unlikely event this happens, your identity could be updated by a hacker without you knowing about it (your identity is either being unlocked or the primary address is changed). But, because the VerusID is locked, the attacker needs to wait until the VerusID unlocks. Meanwhile, you get notified of this update in your VerusID through a DM on Discord (A Telegram Notifier integration is in the pipeline). You then have the time (that you set when you locked the VerusID) to use your Revoke Identity to revoke all access to the staking identity, and then use your Recover identity to update your identity to stay in control of your identity.
Using these [Recover and Revoke capabilities](https://docs.verus.io/verusid/#revoke-recover) of VerusIDs, this also means that Synergo Pool can never run off with your funds without you knowing about it beforehand.

Read more about VerusID [here](https://docs.verus.io/verusid/) and join the [Verus Discord](https://verus.io/discord) to learn more.

**Shares**  
Just like with a PoW mining pool, shares are used to express the work you contributed to staking the next block. Where PoW expresses the hashrate of your miner(s) as shares to keep track, Synergo Pool uses your staking balance as your share. Every new block that is not mined by the pool simply increases your shares with the eligble balance that was staking at that point.
When a block is staked by a member of Synergo Pool, all the shares from everyone up until staking that block are used to divide the block reward among the pool participants, similar to dividing a block reward in a PoW mining pool.