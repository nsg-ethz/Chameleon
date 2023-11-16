FROM rustlang/rust:nightly-buster
RUN apt update

# set the workdir
WORKDIR /chameleon

# Copy everything
COPY . .

# setup ssh
RUN apt update
RUN apt install -y ssh
RUN mkdir -p /root/.ssh

# install coin-cbc
RUN apt install -y coinor-cbc coinor-libcbc-dev

# install the python dependencies
RUN apt install -y python3 python3-pip python3-numpy python3-pandas python3-networkx
RUN pip3 install plotly

# install the toolchain for bgpsim-web
RUN rustup target add wasm32-unknown-unknown
RUN apt install -y ca-certificates curl gnupg
RUN mkdir -p /etc/apt/keyrings
RUN curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg
RUN echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_16.x nodistro main"> /etc/apt/sources.list.d/nodesource.list
# RUN curl -fsSL https://deb.nodesource.com/setup_16.x | bash -
RUN apt install -y nodejs npm
RUN npm install -g tailwindcss
RUN cargo install trunk
RUN cargo install --locked wasm-bindgen-cli

# build the two binaries
RUN cargo install --path . --features "explicit-loop-checker experiment export-web cisco-lab"
RUN cargo install --path . --features "experiment hide-cbc-output" --bin "eval-overhead"
RUN mv /usr/local/cargo/bin/eval-overhead /usr/local/cargo/bin/eval-overhead-implicit
RUN cargo install --path . --features "explicit-loop-checker experiment hide-cbc-output" --bin "eval-overhead"

# prepare the results folder
RUN mkdir ./results

# prepare the PATH variable
ENV PATH="/usr/local/cargo/bin:${PATH}"
