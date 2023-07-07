# Getting the lab setup

This library must know which routers are available under which address, and how many interfaces are available, and how they are connected. To do that, you first need to edit `config/routers.toml`. Write all router names there. Make sure that you have proper `ssh` configuration. Each router must be reachable using `ssh ${router_name}` without any password (using SSH keys). Then, generate the configurations as follows:

```bash
git submodule update --init --remote
cd config
./generate_interfaces.sh
export LAB_SETUP_CONFIG=$(pwd)
```

Also, make sure to export the path to the configuration into the environment variable `LAB_SETUP_CONFIG`.
