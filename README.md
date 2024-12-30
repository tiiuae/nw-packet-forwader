# nw-packet-forwader
Packet forwarder app to forward necessary packets between network interfaces
## Building from Source with Nix

* Clone the repository:

```bash 
git clone https://github.com/tiiuae/nw-packet-forwader.git
cd nw-packet-forwader
```
* Start nix devshell
```bash
nix develop
```

* Build the project
```bash
#release build
nix build .#nwPcktFwdRelease  
#debug build
nix build .#nwPcktFwdDebug
```