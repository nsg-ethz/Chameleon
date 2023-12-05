# SIGCOMM 2023 Artifact Evaluation

This document will show you how to reproduce the results from the paper.
We split the artifact evaluation into two parts:

1. The first part reproduces the evaluation.
   More precisely, you will collect, process, and visualize the data that resulted in Firgure 7, 8, 9, 12, and 13.
   All of those results are obtained by running Chameleon for different scenarios on different topologies from TopologyZoo.
   The binary `eval-overhead` (fond in `src/eval_overhead.rs`) will generate the necessary data, while `analysis/overhead.py` will parse the raw data into a nice format.

2. The second part reproduces the case studies.
   However, we cannot offer public access to our test bed, as it requires physical access and is currently in use to conduct different experiments.
   Nevertheless, we provide the source code that generates the raw data (and the proper command to run it), together with the raw data used in the paper.

## Setup

The artifact evaluation requires you to setup a Docker container.
This container has all dependencies included.
To build that container, execute (this might take around 20 minutes and requires around 5GB of storage):

```shell
docker build -t chameleon .
```

## Evaluation

In the evaluation, we run Chameleon's scheduler on many different topologies, scenarios, and specifications.
We then compute different metrics like scheduling time, reconfiguration time, and memory overhead.
While executing, the program always runs the runtime controller in the simulated network (BgpSim) to validate that the result is actually correct.

### Collecting the data

To collect the data for the evaluation, we perform two main measurements (and one additional for the Figure 13 in the appendix).
The first is where we run the same experiment and the same specification on 106 different topologies.
The second is where we run the same experiment on the same topolgoy for different specifications.

1. **Dataset 1: Different Topologies**:

   ```shell
   docker run -it -v $(pwd)/results:/chameleon/results chameleon eval-overhead --spec=old-until-new-egress --event=del-best-route --timeout=1000000
   ```
   
   This will generate the folder `results/overhead_YYYY-MM-DD_HH-MM-SS`.

   Running this command will take around 12 hours on our server (32 CPU cores and 64GB of memory).
   You will need at least 64GB of memory for the largest topology (KDL).
   To speed up the computation, you can run the evaluation only for topologies with up to `X` nodes (i.e., 100 nodes):

   ```shell
   docker run -it -v $(pwd)/results:/chameleon/results chameleon eval-overhead --spec=old-until-new-egress --event=del-best-route --timeout=1000000 --max=100
   ```
   
2. **Dataset 2: Different Specifications**:

   ```shell
   docker run -it -v $(pwd)/results:/chameleon/results chameleon eval-overhead --spec=iter-spec --topo=Cogentco --event=del-best-route --num-repetitions=20 --timeout=1000000
   ```
   
   This will generate the folder `results/overhead_YYYY-MM-DD_HH-MM-SS`.
   
   Running this command will take around 6 days on our server (32 CPU cores and 64GB of memory).
   To speed up the computation, you can run the evaluation on a smaller topology (like `Colt`), and use only 5 repetitions instead of 20:

   ```shell
   docker run -it -v $(pwd)/results:/chameleon/results chameleon eval-overhead --spec=iter-spec --topo=Colt --event=del-best-route --num-repetitions=5 --timeout=1000000
   ```

### Raw Data Format

Each dataset is stored in the folder `results/overhead_YYYY-MM-DD_HH-MM-SS`.
We recommend you to rename the files to remember which one dataset 1 and dataset 2 is:

```shell
sudo mv results/overhead_YYYY-MM-DD_HH-MM-SS results/overhead_dataset_1
sudo mv results/overhead_YYYY-MM-DD_HH-MM-SS results/overhead_dataset_2
```

Each folder contains a single `json` file for each experiment datapoint (topology, scenario, and event).
The json file has the following top-level objects:
  - `topo`, `spec_builder`, and `scenario` give the information about the specific execution.
  - `net` contains the initial simulated network (before the reconfiguration) as a datastructure that can be imported into BgpSim.
  - `spec` contains the actual generated specification
  - `decomp` contains the reconfiguration plan that Chameleon applies. For the baseline, this will only contain the single reconfiguration command.
  - `data` contains all the results for that datapoint:
    - `data.time`: The time it took Chameleon to compute the reconfiguration plan.
    - `data.result`: Wether Chameleon was successful in computing the reconfiguration plan, and the reconfiguration can be applied in the network.
      - `data.result.Success.cost`: Number of temporary BGP sessions needed
      - `data.result.Success.steps`: Number of steps in the reconfiguration
      - `data.result.Success.max_routes`: Maximum routes in any BGP table throughout the reconfiguration (as simulated)
      - `data.result.Success.routes_before`: Number of routes in any BGP table before the reconfiguration
      - `data.result.Success.routes_after`: Number of routes in any BGP table after the reconfiguration
      - `data.result.Success.max_routes_baseline`: Number of routes in any BGP table when reconfiguring the network using the baseline.
    - `data.num_variables` and `data.num_equations`: The size of the final ILP.
    - `data.model_steps`: Number of steps in the ILP model (should be equal to `data.result.Success.steps`)
    - `data.fw_state_before`: The forwarding state before the reconfiguration.
    - `data.fw_state_after`: The forwarding state after the reconfiguration.

### Data Processing

To process the data, we analyze each datapoint and compute measures based on the `json` file. We compute the following:
- Reconfiguration Complexity (as `potential_deps`) is a function of the initial and final forwarding state, i.e., `data.fw_state_before` and `data.fw_state_after`.
- The specification complexity (as `spec_iter`) is just the number of routers for which there exists waypoint constraints in `spec` (i.e., `spec_builder`).
- The approximate reconfiguration time `est_time`, taking into consideration rounds for which two commands have a direct dependency on each other.
The script will then compute statistics over all repeated datapoints, and store the 10th, 25th, 50th (median), 75th, and 90th percentile in `time_p10`, `time_p25`, `time_p50`, `time_75p`, `time_90p`.

To process the data, run `analysis/overhead.py` (selecting the experiment that you wish to analyze)
You will need to analyze both datasets to generate all plots.

```shell
docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/overhead.py
```

In the following, we explain how to reproduce the plots from the paper:
- **Figure 7**: Run the following on the data set 1:

  ```shell
  docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/plot_reconfiguration_complexity.py
  ```

  This will generate the figure: `results/EXPERIMENT/plot_reconfiguration_complexity.html`, where `EXPERIMENT` is the folder name of the data set 1.

- **Figure 8**: Run the following on the **data set 2**:

  ```shell
  docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/plot_specification_complexity.py
  ```
  
  This will generate the figure: `results/EXPERIMENT/plot_specification_complexity.html`, where `EXPERIMENT` is the folder name of the data set 1. 

- **Figure 9**: Run the following on the data set 1:

  ```shell
  docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/plot_reconfiguration_time.py
  ```
  
  This will generate the figure: `results/EXPERIMENT/plot_reconfiguration_time.html`, where `EXPERIMENT` is the folder name of the data set 1.

- **Figure 12**: Run the following on the data set 1:

  ```shell
  docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/plot_routing_table_size.py
  ```
  
  This will generate the figure: `results/EXPERIMENT/plot_routing_table_size.html`, where `EXPERIMENT` is the folder name of the data set 1.

- **Figure 13**: To replicate Figure 13, you need to re-compute the data set 2 with the binary for which explicit loop checking is disables.
  To do so, run the following command:

  ```shell
  docker run -it -v $(pwd)/results:/chameleon/results chameleon eval-overhead-implicit --spec=iter-spec --topo=Cogentco --event=del-best-route --num-repetitions=20 --timeout=1000000
  ```
  
  (To make the execution faster, you can also choose the topology `Colt` and only `5` repetitions).
  
  Then, generate the same plot as for Figure 8:

  ```shell
  docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/plot_reconfiguration_complexity.py
  ```

  This will generate the figure: `results/EXPERIMENT/plot_reconfiguration_complexity.html`, where `EXPERIMENT` is the folder name of the data set 2.

## Case Study

In the case studies, we perform the reconfiguration in a real network (test bed) and measure the throughput during the reconfiguration.
We compare the performance of Chameleon with the one from Snowcap (the baseline).

### Collecting the data

Since we cannot offer public access to our test bed, we provide the raw data of all case studies found in the paper.
For each case study, we provide the arguments that lead to the plot and the script to analyze the data and plot the results.
The main entry point is `src/main.rs`.

- **Figure 1/6**: 
  - *Files*: `results/lab_baseline_abilene.tar.gz.tar.gz` and `results/lab_chameleon_abilene.tar.gz.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Abilene --spec=old-until-new-egress --event=del-best-route --lab --pecs=1024`
- **Figure 10a**:
  - *Files*: `results/lab_chameleon_abilene_link_failure.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Abilene --spec=old-until-new-egress --event=del-best-route --failure=link-failure --lab --pecs=1024`
- **Figure 10b**: 
  - *Files*: `results/lab_chameleon_abilene_new_best_route.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Abilene --spec=old-until-new-egress --event=del-best-route --failure=new-best-route --lab --pecs=1024`
- **Figure 11a**: 
  - *Files*: `results/lab_baseline_compuserve.tar.gz` and `results/lab_chameleon_compuserve.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Compuserve --spec=old-until-new-egress --event=del-best-route --lab --pecs=1024`
- **Figure 11b**: 
  - *Files*: `results/lab_baseline_hibernia_canada.tar.gz` and `results/lab_chameleon_hibernia_canada.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=HiberniaCanada --spec=old-until-new-egress --event=del-best-route --lab --pecs=1024`
- **Figure 11c**: 
  - *Files*: `results/lab_baseline_sprint.tar.gz` and `results/lab_chameleon_sprint.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Sprint --spec=old-until-new-egress --event=del-best-route --lab --pecs=1024`
- **Figure 11d**: 
  - *Files*: `results/lab_baseline_jgn2plus.tar.gz` and `results/lab_chameleon_jgn2plus.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Jgn2Plus --spec=old-until-new-egress --event=del-best-route --lab --pecs=1024`
- **Figure 11e**: 
  - *Files*: `results/lab_baseline_eenet.tar.gz` and `results/lab_chameleon_eenet.tar.gz`
  - *command*: `docker run -it -e RUST_LOG=info chameleon main --topo=Eenet --spec=old-until-new-egress --event=del-best-route --lab --pecs=1024`
  
We provide all raw data in compressed `/results/*.tar.gz` files.
To extract the data, use `tar`:

```shell
cd results
for f in *.tar.gz; do tar xvf $f; done
```

You can still run the program, just without the test bed.
To do so, omit the two arguments `--lab` and `--pecs=1024`.
The program will then run Chameleon on the simulated network in BgpSim.

### Raw Data Format

Each experiment contains the following files:
- `lab-vdc1XX-NAME.config`: The actual configuration files on all routers in the network.
- `scenario.json`: The raw data that lead to the generated results.
  This json data contains the following top-level objects:
  - `topo`, `spec_builder`, `scenario`, `data.failure`, and `data.pecs` give the information about the specific execution.
  - `net` contains the initial simulated network (before the reconfiguration) as a datastructure that can be imported into BgpSim.
  - `spec` contains the actual generated specification
  - `decomp` contains the reconfiguration plan that Chameleon applies. For the baseline, this will only contain the single reconfiguration command.
- `event.log`: contains the logging of the runtime controller.
  Each log entry descrives when the runtime controller could make progress on some command.
  The same information is also available in `event.json` as a parsable file.
- Several `csv` files that describe the traffic. 
  The file name describes: `{SOURCE-ROUTER-NAME}_{SOURCE-IP}_{DESTINATION-IP}_{EGRESS-ROUTER-NAME}`.
  The CSV lists every packet that was sent by `SOURCE-ROUTER-NAME` for the destination `DESTINATION-PREFIX` that left the network at `EGRESS-ROUTER-NAME`. 
  For each packet, we store the timestamp when the packet was sent, the time it was received, and the identification of that packet. 
  (*Warning*: some measurements only store the time that the packet was received, and store a delta-time since the beginning of the experiment).

### Data Processing

To process the data, we compute the throughput that leaves the network at each egress router.
Further, we compute the number of packets that violate the specification, and the total amount of traffic leaving the network.
To process the data, run `analysis/testbed.py` (selecting the experiment that you wish to analyze)

```shell
docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/testbed.py
```

This will then generate the file `/results/EXPERIMENT/throughput_per_egress.csv`, where `EXPERIMENT` is the folder that stores the experiment results.
To plot the results, execute:

```shell
docker run -it -v $(pwd)/results:/chameleon/results chameleon python3 analysis/plot_testbed.py
```

This will then create the plot as a HTML page: `results/EXPERIMENT/plot_testbed.html`, which you can open with any web browser.
