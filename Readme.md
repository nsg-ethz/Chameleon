[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.8301788.svg)](https://doi.org/10.5281/zenodo.8301788)

# Chameleon: Taming the transient while reconfiguring BGP -- Sigcomm 2023

This is the implementation of the paper “Taming the transient while reconfiguring BGP”, published at SIGCOMM ’23. Please cite the following article:

```bibtex
@INPROCEEDINGS{schneider2023taming,
    year = {2023},
    booktitle = {Proceedings of the 2023 ACM Special Interest Group on Data Communication (SIGCOMM)},
    author = {Schneider, Tibor and Schmid, Roland and Vissicchio, Stefano and Vanbever, Laurent},
    title = {Taming the transient while reconfiguring BGP},
    Note = {37th ACM SiGCOMM Conference (SIGCOMM 2023); Conference Location: New York, NY, USA; Conference Date: September 10-14, 2023}
    doi = {10.1145/3603269.3604855}
    url = {https://doi.org/10.1145/3603269.3604855}
}
```

## Abstract

BGP reconfigurations are a daily occurrence for most network operators, especially in large networks. Despite many recent efforts, performing safe and robust BGP reconfiguration changes is still an open problem. Existing techniques are indeed either (i) unsafe, because they ignore the impact of transient states which can easily lead to invariant violations; or (ii) impractical as they duplicate the entire routing and forwarding states and require hard- and software support.

This paper introduces Chameleon, the first BGP reconfiguration system capable of maintaining correctness throughout the entire reconfiguration process. Chameleon is akin to concurrency coordination in distributed systems. Specifically, we model the reconfiguration process with happens-before relations; show how to reason about (transient) violations; and how to avoid them by precisely controlling BGP route propagation and convergence during the reconfiguration.

We fully implement Chameleon and evaluate it in both testbeds and simulations, on real-world topologies and large-scale reconfiguration scenarios. In most experiments, our system computes reconfiguration plans within a minute, and performs them from start to finish in a few minutes, with minimal overhead and no impact on network resiliency.

## Usage

You can use the library either using your native rust toolchain, or via Docker.
For the artifact evaluation, please consider [this document](sigcomm2023-artifact-evaluation.md)

### Docker

The easiest way is to use the provided Docker file. Build the container using (this might take around 20 minutes):

```shell
docker build -t chameleon .
```

This command will setup the environment, all dependencies, and compile the executable.
(See the [Dockerfile](Dockerfile) for more info.)

You can generate the documentation using:
```shell
docker run -it -v $(pwd)/target:/chameleon/target chameleon cargo doc --all-features
firefox target/doc/chameleon/index.html
```

Then, you can run the program as follows:

```shell
docker run -it chameleon main --help
```

You can increase the log level (from `err` to `info`) by setting the `RUST_LOG` environment variable:

```shell
docker run -it -e RUST_LOG=info chameleon main --help
```

When running experiments, make sure to mount the folder `results` as a volume into the container:

```shell
docker run -it -v $(pwd)/results:/chameleon/results chameleon eval-overhead --help
```


### Native Toolchain

Alternatively, you can setup your own toolchain.
Make sure to install the rust toolchain (using [rustup](https://rustup.rs)). Make sure you use the nightly toolchain:

```shell
rustup toolchain install nightly
rustup toolchain default nightly
```

Additionally, install coinor cbc (the library) under Ubuntu:

```shell
sudo apt-get install coinor-cbc coinor-libcbc-dev
```

To run the data analysis, install all python requirements listed in `analysis/requirements.txt`:
```shell
cd analysis
python -m venv ./.env
source ./.env/bin/activate
pip install --requirement requirements.txt
```

## Web Application

This repository also contains the code for the web application to run the simulator.
The simulator is built as a client-side WASM application, and thus does not require any special server to run it.
The web application is hosted at [bgpsim.github.io](https://bgpsim.github.io).
If the website is down, you can build the web application from source:

To run the application locally (at `http://127.0.0.1:8080/`), do:

```shell
docker run -it -w /chameleon/bgpsim-web --network host chameleon trunk serve --all-features --release
```

To build the application to static HTML, CSS, JavaScript and WASM files, run the following command:

```shell
docker run -it -w /chameleon/bgpsim-web -v $(pwd)/bgpsim-web/dist:/chameleon/bgpsim-web/dist chameleon trunk build --all-features --release
```

This command will generate the folder `bgpsim-web/dist` with all needed files. 
You can simply serve those files from any simple HTTP server (static web server).
