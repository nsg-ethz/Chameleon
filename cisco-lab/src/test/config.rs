// BgpSim: BGP Network Simulator written in Rust
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

#[generic_tests::define]
mod t {

    use bgpsim::builder::{constant_link_weight, equal_preferences, NetworkBuilder};
    use bgpsim::export::{Addressor, DefaultAddressor};
    use bgpsim::prelude::*;
    use itertools::Itertools;
    use pretty_assertions::assert_eq;

    use crate::CiscoLab;

    fn fix_addressor<P: Prefix, Q>(addressor: &mut DefaultAddressor<'_, P, Q>) {
        for a in 0..4 {
            let a = RouterId::from(a);
            addressor.router(a).unwrap();
            for b in 0..4 {
                let b = RouterId::from(b);
                addressor.iface(a, b).unwrap();
                addressor.iface(b, a).unwrap();
            }
        }
        for (int, ext) in [(0, 4), (1, 5)] {
            let int = RouterId::from(int);
            let ext = RouterId::from(ext);
            addressor.iface(int, ext).unwrap();
            addressor.iface(ext, int).unwrap();
        }
    }

    fn test_net<P: Prefix>() -> Network<P, BasicEventQueue<P>> {
        let mut net = Network::build_complete_graph(BasicEventQueue::<P>::new(), 4);
        net.build_external_routers(|_, _| vec![0.into(), 1.into()], ())
            .unwrap();
        net.build_link_weights(constant_link_weight, 10.0).unwrap();
        net.build_ebgp_sessions().unwrap();
        net.build_ibgp_full_mesh().unwrap();
        net.build_advertisements(P::from(0), equal_preferences, 2)
            .unwrap();
        net
    }

    #[test]
    fn config_router_0<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        let cfg = lab.generate_router_config(0.into()).unwrap();
        assert_eq!(&cfg, include_str!("files/test_net_router_0.conf"));
    }

    #[test]
    fn config_router_2<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        let cfg = lab.generate_router_config(2.into()).unwrap();
        assert_eq!(&cfg, include_str!("files/test_net_router_2.conf"));
    }

    #[test]
    fn all_router_configs<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        let cfg = lab.generate_router_config_all().unwrap();
        assert_eq!(cfg.values().map(|(r, _)| r).unique().count(), 4);
    }

    #[test]
    fn exabgp_config<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        let cfg = lab.generate_exabgp_config().unwrap();
        assert_eq!(cfg, include_str!("files/test_net_exabgp.conf"));
    }

    #[test]
    fn exabgp_script<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        let script = lab.generate_exabgp_runner().unwrap();
        assert_eq!(script, include_str!("files/test_net_exabgp.py"));
    }

    #[test]
    fn exabgp_script_withdraw<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        lab.step_external_time();
        lab.withdraw_route(4.into(), P::from(0)).unwrap();
        let script = lab.generate_exabgp_runner().unwrap();
        assert_eq!(script, include_str!("files/test_net_exabgp_withdraw.py"));
    }

    #[test]
    fn exabgp_netplan_config<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        let _ = lab.generate_exabgp_config().unwrap();
        let cfg = lab.generate_exabgp_netplan_config().unwrap();
        assert_eq!(cfg, include_str!("files/test_net_netplan.conf"));
    }

    #[test]
    fn tofino_controller<P: Prefix>() {
        let net = test_net::<P>();
        let mut lab = CiscoLab::new(&net).unwrap();
        fix_addressor(lab.addressor_mut());
        let script = lab.generate_tofino_controller().unwrap();
        // in the script, ignore the content of rules_static_route
        let mut script_mod = String::new();
        let mut ignore_line = false;
        for line in script.lines() {
            if !ignore_line {
                script_mod.push_str(line);
                script_mod.push('\n');
                if line == "rules_static_route = {" {
                    ignore_line = true;
                }
            } else if line == "}" {
                ignore_line = false;
                script_mod.push_str("}\n");
            }
        }
        assert_eq!(script_mod, include_str!("files/controller.py"));
    }

    #[instantiate_tests(<SinglePrefix>)]
    mod single {}

    #[instantiate_tests(<SimplePrefix>)]
    mod simple {}
}
