# Installation instructions

These instructions install the back end of the staking pool. The staking pool does not come with an interface, as it is not strictly required to have one. Anyone can update their VerusID with the necessary information in order to stake.

## Manual installation (Debian / Ubuntu)

The following services are needed in order to run the pool:

- postgres
- zeromq
- rust
- Verus native daemon

### Server setup

A server with at least 6G of RAM is required, or set some swapspace in case it's around 6G. Or don't start the daemon with fastload, but this significantly increases the startup time (more than 30 minutes)

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

Add the following config to the file

```sh
rpcuser=<a random username>
rpcpassword=<a random password>
rpcport=27486
server=1
txindex=1
rpcworkqueue=256
rpcallowip=127.0.0.1
rpchost=127.0.0.1

zmqpubhashblock=tcp://127.0.0.1:59790
```

Save and exit.

Start the Verus daemon and let it load, it can take 30+ min. 

```
verusd -daemon
```

If you have plenty of RAM, start the daemon with the `-fastload` flag.


### Pool

#### Rust installation

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh 
```

#### Postgres installation

There are many ways to use the postgresql database. You can install it manually, you can use a third party cloud provider like [railway.app](https://railway.app), or you can use Docker, although Docker in production is generally discouraged.

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

