# rockscrew

Port of [corkscrew](http://www.agroman.net/corkscrew/) in Rust, inspired by [corkscrew-rs](https://github.com/yageek/corkscrew-rs).

## Installation

```bash
$ cargo install rockscrew
```

## Usage with SSH

In your `~/.ssh/config`:

```
ProxyCommand rockscrew proxy.example.com 80 %h %p ~/.ssh/myauth
```
