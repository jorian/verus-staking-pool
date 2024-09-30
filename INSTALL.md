# Installation instructions

These instructions install the back end of the staking pool. The staking pool does not come with an interface, as it is not strictly required to have one. Anyone can update their VerusID with the necessary information in order to stake.

## Manual installation (Debian / Ubuntu)

The following services are needed in order to run the pool:

- postgres
- zeromq
- rust
- Verus native daemon

### Server setup

A server with at least 10G of RAM is required, or set some swap space in case it's around 8G. (Or don't start the daemon with fastload, but this significantly increases the startup time with at least 45 minutes, depending on how beefy the server hardware is)

```sh
apt update
apt -y upgrade
apt -y install libzmq3-dev pkg-config libssl-dev libgomp1 git libboost-all-dev libsodium-dev build-essential ca-certificates curl gnupg lsb-release
```

Let's make two users, one for the verus daemon and one for the pool

```sh
useradd -m -d /home/verus -s /bin/bash verus
useradd -m -d /home/pool -s /bin/bash pool
su - verus
```

### Verus
(Note: the latest Verus version might differ from the one below, please change the command when there is a newer version)
```sh
wget https://github.com/VerusCoin/VerusCoin/releases/download/v1.2.4/Verus-CLI-Linux-v1.2.4-x86_64.tgz
tar xf Verus-CLI-Linux-v1.2.4-x86_64.tgz 
tar xf Verus-CLI-Linux-v1.2.4-x86_64.tar.gz
mv verus-cli/{fetch-params,fetch-bootstrap,verusd,verus} ~/bin
rm -rf verus-cli Verus-CLI-Linux*
```

```sh
cd bin
./fetch-bootstrap
./fetch-params
```

The daemon config needs to be prepared. The pool uses ZMQ to get notified of new blocks, so we need to set that up.

```
cd ~/.komodo/VRSC
nano VRSC.conf
```

Add the following config to the file. You can use the output of this command to create a reasonably secure password:

`cat /dev/urandom | tr -dc 'a-zA-Z0-9' | fold -w 32 | head -n 1`

```sh
rpcuser=<a username>
rpcpassword=<a secure password>
rpcport=27486
server=1
txindex=1
rpcworkqueue=256
rpcallowip=127.0.0.1
rpchost=127.0.0.1

zmqpubhashblock=tcp://127.0.0.1:59790
```

Make sure to change the user and password fields. Save and exit.

Start the Verus daemon and let it load, it can take 30+ min. 

```
verusd -daemon
```

If you have plenty of RAM, start the daemon with the `-fastload` flag.

Let's now set up the staking pool

### Pool

We are going to install the pool from source.

#### Rust installation

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh 
```

#### Postgres installation

There are many ways to use a postgresql database. You can install it manually, you can use a third party cloud provider like [railway.app](https://railway.app), or you can use Docker, although Docker in production is generally discouraged, because it adds a layer of complexity.

In a new tmux pane / ssh session, as root:

```sh
mkdir -p /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
echo \
  "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/debian \
  $(lsb_release -cs) stable" | sudo tee /etc/apt/sources.list.d/docker.list > /dev/null
apt-get update
apt-get install docker-ce docker-ce-cli containerd.io docker-compose-plugin
usermod -aG docker pool
su - pool
docker pull postgres:alpine
```

Clone this repo:
```
cd ~/
git clone https://github.com/jorian/verus-staking-pool
```

In the repository that was just cloned, example configuration files have been included. Copy those files to the correct folder and update the files accordingly.

```
cd ~/verus-staking-pool
cp coin_config/vrsctest.toml.example coin_config/vrsctest.toml
cp config/base.json.example config/base.json
```

In `coin_config/vrsctest.toml` you should update the following fields:

```sh
- pool_address
- pool_primary_address 
- fee
- min_payout
- tx_fee
- rpc_user
- rpc_password
- rpc_host
- rpc_port
- zmq_port_blocknotify
```

`pool_address` is the i-address of the identity that is used to collect the staking rewards and to send rewards from to the stakers. The daemon will need to be started with `defaultid=<pool_address>`. It must be a VerusID.

`pool_primary_address` is the R-address that people will use to join the staking pool. It should be an address that is owned by the wallet on the machine
you're installing the pool on. (Generally, you would go to the verus user, do a `verus getnewaddress`, backup the private key, and use it as the pool's primary address).

Create a new password for the postgres instance and use it in the config file you are about to update

`cat /dev/urandom | tr -dc 'a-zA-Z0-9' | fold -w 32 | head -n 1`

In `config/base.json` you should update the database section to your situation. Use the password you just generated
as the database password.

Now with that same password, start the postgres instance, replacing `<password>`.

```sh
docker run --name postgres -e POSTGRES_PASSWORD=<password> -d -p 5432:5432 postgres:alpine
```

Now, we need to create the database:

`DATABASE_URL=postgres://postgres:<postgres_password>@127.0.0.1:5432/<name of database> cargo sqlx database create`

and apply the migrations

`DATABASE_URL=postgres://postgres:<postgres_password>@127.0.0.1:5432/<name of database> cargo sqlx migrate run --source=pool/sql/migrations`

Everytime you update the pool and new database functionality was added, you need to run this `cargo sqlx migrate run` command, to make the
database aware of new changes.

To be able to compile, we need to use this same DATABASE_URL. Let's put it in a `.env` file to make life easier:

```
echo 'DATABASE_URL=postgres://postgres:<postgres_password>@127.0.0.1:5432/<name of database>' >> .env
```

Now compile should work.

`cargo build` 

