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

    use crate::{
        bgp::BgpSessionType::*,
        event::{EventQueue, ModelParams, SimpleTimingModel},
        network::Network,
        types::{AsId, Ipv4Prefix, NetworkError, Prefix, RouterId, SimplePrefix, SinglePrefix},
    };
    use pretty_assertions::assert_eq;

    /// Setup the simple network, and return `(e0, b0, r0, r1, b1, e1)`
    /// All weights are 1, r0 and b0 form a iBGP cluster, and so does r1 and b1
    ///
    /// r0 ----- r1
    /// |        |
    /// |        |
    /// b0       b1   internal
    /// |........|............
    /// |        |    external
    /// e0       e1
    fn setup_simple<P, Q>(
        net: &mut Network<P, Q>,
    ) -> (RouterId, RouterId, RouterId, RouterId, RouterId, RouterId)
    where
        P: Prefix,
        Q: EventQueue<P>,
    {
        let e0 = net.add_external_router("E0", AsId(1));
        let b0 = net.add_router("B0");
        let r0 = net.add_router("R0");
        let r1 = net.add_router("R1");
        let b1 = net.add_router("B1");
        let e1 = net.add_external_router("E1", AsId(1));

        net.add_link(e0, b0);
        net.add_link(b0, r0);
        net.add_link(r0, r1);
        net.add_link(r1, b1);
        net.add_link(b1, e1);

        net.set_link_weight(e0, b0, 1.0).unwrap();
        net.set_link_weight(b0, e0, 1.0).unwrap();
        net.set_link_weight(b0, r0, 1.0).unwrap();
        net.set_link_weight(r0, b0, 1.0).unwrap();
        net.set_link_weight(r0, r1, 1.0).unwrap();
        net.set_link_weight(r1, r0, 1.0).unwrap();
        net.set_link_weight(r1, b1, 1.0).unwrap();
        net.set_link_weight(b1, r1, 1.0).unwrap();
        net.set_link_weight(b1, e1, 1.0).unwrap();
        net.set_link_weight(e1, b1, 1.0).unwrap();
        net.set_bgp_session(e0, b0, Some(EBgp)).unwrap();
        net.set_bgp_session(r0, b0, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r0, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, b1, Some(IBgpClient)).unwrap();
        net.set_bgp_session(e1, b1, Some(EBgp)).unwrap();

        (e0, b0, r0, r1, b1, e1)
    }

    #[test]
    fn test_undo_all<P: Prefix>() {
        let mut net: Network<P, _> = Network::default();
        let prefix = P::from(0);
        let net_hist_1 = net.clone();
        let e0 = net.add_external_router("E0", AsId(1));
        let net_hist_2 = net.clone();
        let b0 = net.add_router("B0");
        let net_hist_3 = net.clone();
        let r0 = net.add_router("R0");
        let net_hist_4 = net.clone();
        let r1 = net.add_router("R1");
        let net_hist_5 = net.clone();
        let b1 = net.add_router("B1");
        let net_hist_6 = net.clone();
        let e1 = net.add_external_router("E1", AsId(1));
        let net_hist_7 = net.clone();

        net.add_link(e0, b0);
        let net_hist_8 = net.clone();
        net.add_link(b0, r0);
        let net_hist_9 = net.clone();
        net.add_link(r0, r1);
        let net_hist_10 = net.clone();
        net.add_link(r1, b1);
        let net_hist_11 = net.clone();
        net.add_link(b1, e1);
        let net_hist_12 = net.clone();

        net.set_link_weight(e0, b0, 1.0).unwrap();
        let net_hist_13 = net.clone();
        net.set_link_weight(b0, e0, 1.0).unwrap();
        let net_hist_14 = net.clone();
        net.set_link_weight(b0, r0, 1.0).unwrap();
        let net_hist_15 = net.clone();
        net.set_link_weight(r0, b0, 1.0).unwrap();
        let net_hist_16 = net.clone();
        net.set_link_weight(r0, r1, 1.0).unwrap();
        let net_hist_17 = net.clone();
        net.set_link_weight(r1, r0, 1.0).unwrap();
        let net_hist_18 = net.clone();
        net.set_link_weight(r1, b1, 1.0).unwrap();
        let net_hist_19 = net.clone();
        net.set_link_weight(b1, r1, 1.0).unwrap();
        let net_hist_20 = net.clone();
        net.set_link_weight(b1, e1, 1.0).unwrap();
        let net_hist_21 = net.clone();
        net.set_link_weight(e1, b1, 1.0).unwrap();
        let net_hist_22 = net.clone();
        net.set_bgp_session(e0, b0, Some(EBgp)).unwrap();
        let net_hist_23 = net.clone();
        net.set_bgp_session(r0, b0, Some(IBgpClient)).unwrap();
        let net_hist_24 = net.clone();
        net.set_bgp_session(r0, r1, Some(IBgpPeer)).unwrap();
        let net_hist_25 = net.clone();
        net.set_bgp_session(r1, b1, Some(IBgpClient)).unwrap();
        let net_hist_26 = net.clone();
        net.set_bgp_session(e1, b1, Some(EBgp)).unwrap();
        let net_hist_27 = net.clone();
        net.advertise_external_route(e0, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        let net_hist_28 = net.clone();

        net.advertise_external_route(e1, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_28);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_27);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_26);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_25);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_24);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_23);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_22);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_21);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_20);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_19);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_18);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_17);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_16);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_15);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_14);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_13);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_12);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_11);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_10);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_9);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_8);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_7);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_6);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_5);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_4);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn test_undo_marks<P: Prefix>() {
        let mut net: Network<P, _> = Network::default();
        let prefix = P::from(0);
        let net_hist_0 = net.clone();
        let mark_0 = net.get_undo_mark();
        let e0 = net.add_external_router("E0", AsId(1));
        let b0 = net.add_router("B0");
        let r0 = net.add_router("R0");
        let net_hist_1 = net.clone();
        let mark_1 = net.get_undo_mark();
        let r1 = net.add_router("R1");
        let b1 = net.add_router("B1");
        let e1 = net.add_external_router("E1", AsId(1));

        net.add_link(e0, b0);
        net.add_link(b0, r0);
        let net_hist_2 = net.clone();
        let mark_2 = net.get_undo_mark();
        net.add_link(r0, r1);
        net.add_link(r1, b1);
        net.add_link(b1, e1);

        net.set_link_weight(e0, b0, 1.0).unwrap();
        net.set_link_weight(b0, e0, 1.0).unwrap();
        net.set_link_weight(b0, r0, 1.0).unwrap();
        net.set_link_weight(r0, b0, 1.0).unwrap();
        let net_hist_3 = net.clone();
        let mark_3 = net.get_undo_mark();
        net.set_link_weight(r0, r1, 1.0).unwrap();
        net.set_link_weight(r1, r0, 1.0).unwrap();
        net.set_link_weight(r1, b1, 1.0).unwrap();
        net.set_link_weight(b1, r1, 1.0).unwrap();
        net.set_link_weight(b1, e1, 1.0).unwrap();
        net.set_link_weight(e1, b1, 1.0).unwrap();
        net.set_bgp_session(e0, b0, Some(EBgp)).unwrap();
        net.set_bgp_session(r0, b0, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r0, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, b1, Some(IBgpClient)).unwrap();
        let net_hist_4 = net.clone();
        let mark_4 = net.get_undo_mark();
        net.set_bgp_session(e1, b1, Some(EBgp)).unwrap();

        net.advertise_external_route(e0, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        net.advertise_external_route(e1, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();

        net.undo_to_mark(mark_4).unwrap();
        assert_eq!(net, net_hist_4);
        net.undo_to_mark(mark_3).unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_to_mark(mark_2).unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_to_mark(mark_1).unwrap();
        assert_eq!(net, net_hist_1);
        net.undo_to_mark(mark_0).unwrap();
        assert_eq!(net, net_hist_0);
    }

    #[test]
    fn test_simple<P: Prefix>() {
        let mut net: Network<P, _> = Network::default();
        let prefix = P::from(0);

        let (e0, _, _, _, _, e1) = setup_simple(&mut net);
        let net_hist_1 = net.clone();

        // advertise the same prefix on both routers
        net.advertise_external_route(e0, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();

        net.advertise_external_route(e1, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn test_simple_model<P: Prefix>() {
        let mut net: Network<P, _> = Network::new(SimpleTimingModel::new(ModelParams::new(
            0.1, 1.0, 2.0, 5.0, 0.1,
        )));

        let prefix = P::from(0);

        let (e0, _, _, _, _, e1) = setup_simple(&mut net);
        let net_hist_1 = net.clone();

        // advertise the same prefix on both routers
        net.advertise_external_route(e0, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(e1, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    /// Setup the second network, and return `(e1, r1, r2, r3, r4, e4)`
    /// - All IGP weights are set to 1, except r3 -- r4: 2
    /// - BGP sessions before:
    ///   - e1 <-> r1
    ///   - r1 <-> r2
    ///   - r1 <-> r3
    ///   - r1 <-> r4
    /// - BGP sessions after:
    ///   - e4 <-> r4
    ///   - r4 <-> r1
    ///   - r4 <-> r2
    ///   - r4 <-> r3
    ///
    ///  e1 ---- r1 ---- r2
    ///          |    .-'|
    ///          | .-'   |
    ///          r3 ---- r4 ---- e4
    fn setup_external<P, Q>(
        net: &mut Network<P, Q>,
    ) -> (RouterId, RouterId, RouterId, RouterId, RouterId, RouterId)
    where
        P: Prefix,
        Q: EventQueue<P>,
    {
        // add routers
        let r1 = net.add_router("r1");
        let r2 = net.add_router("r2");
        let r3 = net.add_router("r3");
        let r4 = net.add_router("r4");
        let e1 = net.add_external_router("e1", AsId(65101));
        let e4 = net.add_external_router("e4", AsId(65104));

        // add links
        net.add_link(r1, r2);
        net.add_link(r1, r3);
        net.add_link(r2, r3);
        net.add_link(r2, r4);
        net.add_link(r3, r4);
        net.add_link(r1, e1);
        net.add_link(r4, e4);

        // prepare the configuration
        net.set_link_weight(r1, r2, 1.0).unwrap();
        net.set_link_weight(r1, r3, 1.0).unwrap();
        net.set_link_weight(r2, r3, 1.0).unwrap();
        net.set_link_weight(r2, r4, 1.0).unwrap();
        net.set_link_weight(r3, r4, 2.0).unwrap();
        net.set_link_weight(r1, e1, 1.0).unwrap();
        net.set_link_weight(r4, e4, 1.0).unwrap();
        net.set_link_weight(r2, r1, 1.0).unwrap();
        net.set_link_weight(r3, r1, 1.0).unwrap();
        net.set_link_weight(r3, r2, 1.0).unwrap();
        net.set_link_weight(r4, r2, 1.0).unwrap();
        net.set_link_weight(r4, r3, 2.0).unwrap();
        net.set_link_weight(e1, r1, 1.0).unwrap();
        net.set_link_weight(e4, r4, 1.0).unwrap();
        net.set_bgp_session(r1, e1, Some(EBgp)).unwrap();
        net.set_bgp_session(r1, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r4, Some(IBgpPeer)).unwrap();

        (e1, r1, r2, r3, r4, e4)
    }

    #[test]
    fn test_external_router<P: Prefix>() {
        let mut net: Network<P, _> = Network::default();
        let prefix = P::from(0);

        let (e1, r1, r2, r3, r4, e4) = setup_external(&mut net);

        let net_hist_1 = net.clone();

        // advertise routes
        net.advertise_external_route(e1, prefix, vec![AsId(65101), AsId(65200)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(e4, prefix, vec![AsId(65104), AsId(65200)], None, None)
            .unwrap();
        let net_hist_3 = net.clone();

        // insert new sessions
        net.set_bgp_session(r2, r4, Some(IBgpPeer)).unwrap();
        let net_hist_4 = net.clone();
        net.set_bgp_session(r3, r4, Some(IBgpPeer)).unwrap();
        let net_hist_5 = net.clone();
        net.set_bgp_session(r4, e4, Some(EBgp)).unwrap();
        let net_hist_6 = net.clone();

        // remove all old sessions
        net.set_bgp_session(r1, r2, None).unwrap();
        let net_hist_7 = net.clone();
        net.set_bgp_session(r1, r3, None).unwrap();
        let net_hist_8 = net.clone();
        net.set_bgp_session(r1, e1, None).unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_8);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_7);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_6);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_5);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_4);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn test_external_router_model<P: Prefix>() {
        let mut net: Network<P, _> = Network::new(SimpleTimingModel::new(ModelParams::new(
            0.1, 1.0, 2.0, 5.0, 0.1,
        )));
        let prefix = P::from(0);

        let (e1, r1, r2, r3, r4, e4) = setup_external(&mut net);
        let net_hist_1 = net.clone();

        // advertise routes
        net.advertise_external_route(e1, prefix, vec![AsId(65101), AsId(65200)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(e4, prefix, vec![AsId(65104), AsId(65200)], None, None)
            .unwrap();
        let net_hist_3 = net.clone();

        // insert new sessions
        net.set_bgp_session(r2, r4, Some(IBgpPeer)).unwrap();
        let net_hist_4 = net.clone();
        net.set_bgp_session(r3, r4, Some(IBgpPeer)).unwrap();
        let net_hist_5 = net.clone();
        net.set_bgp_session(r4, e4, Some(EBgp)).unwrap();
        let net_hist_6 = net.clone();

        // remove all old sessions
        net.set_bgp_session(r1, r2, None).unwrap();
        let net_hist_7 = net.clone();
        net.set_bgp_session(r1, r3, None).unwrap();
        let net_hist_8 = net.clone();
        net.set_bgp_session(r1, e1, None).unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_8);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_7);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_6);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_5);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_4);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn test_route_order1<P: Prefix>() {
        // All weights are 1
        // r0 and b0 form a iBGP cluster, and so does r1 and b1
        //
        // r0 ----- r1
        // |        |
        // |        |
        // b1       b0   internal
        // |........|............
        // |        |    external
        // e1       e0
        let mut net: Network<P, _> = Network::default();

        let prefix = P::from(0);

        let e0 = net.add_external_router("E0", AsId(1));
        let b0 = net.add_router("B0");
        let r0 = net.add_router("R0");
        let r1 = net.add_router("R1");
        let b1 = net.add_router("B1");
        let e1 = net.add_external_router("E1", AsId(1));

        net.add_link(e0, b0);
        net.add_link(b0, r1);
        net.add_link(r0, r1);
        net.add_link(r0, b1);
        net.add_link(b1, e1);

        net.set_link_weight(e0, b0, 1.0).unwrap();
        net.set_link_weight(b0, e0, 1.0).unwrap();
        net.set_link_weight(b0, r1, 1.0).unwrap();
        net.set_link_weight(r1, b0, 1.0).unwrap();
        net.set_link_weight(r0, r1, 1.0).unwrap();
        net.set_link_weight(r1, r0, 1.0).unwrap();
        net.set_link_weight(r0, b1, 1.0).unwrap();
        net.set_link_weight(b1, r0, 1.0).unwrap();
        net.set_link_weight(b1, e1, 1.0).unwrap();
        net.set_link_weight(e1, b1, 1.0).unwrap();
        net.set_bgp_session(e0, b0, Some(EBgp)).unwrap();
        net.set_bgp_session(r0, b0, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r0, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, b1, Some(IBgpClient)).unwrap();
        net.set_bgp_session(e1, b1, Some(EBgp)).unwrap();

        let net_hist_1 = net.clone();

        // advertise the same prefix on both routers
        net.advertise_external_route(e0, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(e1, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn test_route_order2<P: Prefix>() {
        // All weights are 1
        // r0 and b0 form a iBGP cluster, and so does r1 and b1
        //
        // r0 ----- r1
        // |        |
        // |        |
        // b1       b0   internal
        // |........|............
        // |        |    external
        // e1       e0
        let mut net: Network<P, _> = Network::default();

        let prefix = P::from(0);

        let e0 = net.add_external_router("E0", AsId(1));
        let b0 = net.add_router("B0");
        let r0 = net.add_router("R0");
        let r1 = net.add_router("R1");
        let b1 = net.add_router("B1");
        let e1 = net.add_external_router("E1", AsId(1));

        net.add_link(e0, b0);
        net.add_link(b0, r1);
        net.add_link(r0, r1);
        net.add_link(r0, b1);
        net.add_link(b1, e1);

        net.set_link_weight(e0, b0, 1.0).unwrap();
        net.set_link_weight(b0, e0, 1.0).unwrap();
        net.set_link_weight(b0, r1, 1.0).unwrap();
        net.set_link_weight(r1, b0, 1.0).unwrap();
        net.set_link_weight(r0, r1, 1.0).unwrap();
        net.set_link_weight(r1, r0, 1.0).unwrap();
        net.set_link_weight(r0, b1, 1.0).unwrap();
        net.set_link_weight(b1, r0, 1.0).unwrap();
        net.set_link_weight(b1, e1, 1.0).unwrap();
        net.set_link_weight(e1, b1, 1.0).unwrap();
        net.set_bgp_session(e0, b0, Some(EBgp)).unwrap();
        net.set_bgp_session(r0, b0, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r0, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, b1, Some(IBgpClient)).unwrap();
        net.set_bgp_session(e1, b1, Some(EBgp)).unwrap();

        // advertise the same prefix on both routers
        let net_hist_1 = net.clone();
        net.advertise_external_route(e1, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(e0, prefix, vec![AsId(1), AsId(2), AsId(3)], None, None)
            .unwrap();
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn test_bad_gadget<P: Prefix>() {
        // weights between ri and bi are 5, weights between ri and bi+1 are 1
        // ri and bi form a iBGP cluster
        //
        //    _________________
        //  /                  \
        // |  r0       r1       r2
        // |  | '-.    | '-.    |
        //  \ |    '-. |    '-. |
        //    b0       b1       b2   internal
        //    |........|........|............
        //    |        |        |external
        //    e0       e1       e2
        let mut net: Network<P, _> = Network::default();

        let prefix = P::from(0);

        let e0 = net.add_external_router("E0", AsId(65100));
        let e1 = net.add_external_router("E1", AsId(65101));
        let e2 = net.add_external_router("E2", AsId(65102));
        let b0 = net.add_router("B0");
        let b1 = net.add_router("B1");
        let b2 = net.add_router("B2");
        let r0 = net.add_router("R0");
        let r1 = net.add_router("R1");
        let r2 = net.add_router("R2");

        net.add_link(e0, b0);
        net.add_link(e1, b1);
        net.add_link(e2, b2);
        net.add_link(b0, r0);
        net.add_link(b1, r1);
        net.add_link(b2, r2);
        net.add_link(r0, b1);
        net.add_link(r1, b2);
        net.add_link(r2, b0);

        net.set_link_weight(e0, b0, 1.0).unwrap();
        net.set_link_weight(b0, e0, 1.0).unwrap();
        net.set_link_weight(e1, b1, 1.0).unwrap();
        net.set_link_weight(b1, e1, 1.0).unwrap();
        net.set_link_weight(e2, b2, 1.0).unwrap();
        net.set_link_weight(b2, e2, 1.0).unwrap();
        net.set_link_weight(b0, r0, 5.0).unwrap();
        net.set_link_weight(r0, b0, 5.0).unwrap();
        net.set_link_weight(b1, r1, 5.0).unwrap();
        net.set_link_weight(r1, b1, 5.0).unwrap();
        net.set_link_weight(b2, r2, 5.0).unwrap();
        net.set_link_weight(r2, b2, 5.0).unwrap();
        net.set_link_weight(r0, b1, 1.0).unwrap();
        net.set_link_weight(b1, r0, 1.0).unwrap();
        net.set_link_weight(r1, b2, 1.0).unwrap();
        net.set_link_weight(b2, r1, 1.0).unwrap();
        net.set_link_weight(r2, b0, 1.0).unwrap();
        net.set_link_weight(b0, r2, 1.0).unwrap();
        net.set_bgp_session(r0, b0, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r1, b1, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r2, b2, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r0, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r0, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(b0, e0, Some(EBgp)).unwrap();
        net.set_bgp_session(b1, e1, Some(EBgp)).unwrap();
        net.set_bgp_session(b2, e2, Some(EBgp)).unwrap();

        net.set_msg_limit(Some(1000));

        // advertise the same prefix on both routers
        let net_hist_1 = net.clone();
        net.advertise_external_route(e2, prefix, vec![AsId(0), AsId(1)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(e1, prefix, vec![AsId(0), AsId(1)], None, None)
            .unwrap();

        let net_hist_3 = net.clone();
        let last_advertisement =
            net.advertise_external_route(e0, prefix, vec![AsId(0), AsId(1)], None, None);
        assert!(last_advertisement == Err(NetworkError::NoConvergence));

        net.queue.clear();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn change_ibgp_topology_1<P: Prefix>() {
        // Example from L. Vanbever bgpmig_ton, figure 1
        //
        // igp topology
        //
        // rr is connected to e1, e2, e3 with weights 1, 2, 3 respectively. Assymetric: back direction has weight 100
        // ri is connected to ei with weight 10
        // ri is connected to ei-1 with weight 1
        //
        //    _________________
        //  /                  \
        // |  r3       r2       r1
        // |  | '-.    | '-.    |
        //  \ |    '-. |    '-. |
        //    e3       e2       e1   internal
        //    |........|........|............
        //    |        |        |    external
        //    p3       p2       p1
        //
        // ibgp start topology
        // .-----------------------.
        // |   rr   r1   r2   r3   | full mesh
        // '--------^----^---/^----'
        //          |    |.-' |
        //          e1   e2   e3
        //
        // ibgp end topology
        //
        //         .-rr-.
        //        /  |   \
        //       /   |    \
        //      r1   r2   r3
        //      |    |    |
        //      e1   e2   e3

        let mut net: Network<P, _> = Network::default();

        let prefix = P::from(0);

        let rr = net.add_router("rr");
        let r1 = net.add_router("r1");
        let r2 = net.add_router("r2");
        let r3 = net.add_router("r3");
        let e1 = net.add_router("e1");
        let e2 = net.add_router("e2");
        let e3 = net.add_router("e3");
        let p1 = net.add_external_router("p1", AsId(65101));
        let p2 = net.add_external_router("p2", AsId(65102));
        let p3 = net.add_external_router("p3", AsId(65103));

        net.add_link(r1, e1);
        net.add_link(r2, e2);
        net.add_link(r3, e3);
        net.add_link(e1, p1);
        net.add_link(e2, p2);
        net.add_link(e3, p3);
        net.add_link(e1, r2);
        net.add_link(e2, r3);
        net.add_link(e3, r1);
        net.add_link(rr, e1);
        net.add_link(rr, e2);
        net.add_link(rr, e3);

        net.set_link_weight(r1, e1, 10.0).unwrap();
        net.set_link_weight(e1, r1, 10.0).unwrap();
        net.set_link_weight(r2, e2, 10.0).unwrap();
        net.set_link_weight(e2, r2, 10.0).unwrap();
        net.set_link_weight(r3, e3, 10.0).unwrap();
        net.set_link_weight(e3, r3, 10.0).unwrap();
        net.set_link_weight(e1, p1, 1.0).unwrap();
        net.set_link_weight(p1, e1, 1.0).unwrap();
        net.set_link_weight(e2, p2, 1.0).unwrap();
        net.set_link_weight(p2, e2, 1.0).unwrap();
        net.set_link_weight(e3, p3, 1.0).unwrap();
        net.set_link_weight(p3, e3, 1.0).unwrap();
        net.set_link_weight(e1, r2, 1.0).unwrap();
        net.set_link_weight(r2, e1, 1.0).unwrap();
        net.set_link_weight(e2, r3, 1.0).unwrap();
        net.set_link_weight(r3, e2, 1.0).unwrap();
        net.set_link_weight(e3, r1, 1.0).unwrap();
        net.set_link_weight(r1, e3, 1.0).unwrap();
        net.set_link_weight(rr, e1, 1.0).unwrap();
        net.set_link_weight(e1, rr, 100.0).unwrap();
        net.set_link_weight(rr, e2, 2.0).unwrap();
        net.set_link_weight(e2, rr, 100.0).unwrap();
        net.set_link_weight(rr, e3, 3.0).unwrap();
        net.set_link_weight(e3, rr, 100.0).unwrap();
        net.set_bgp_session(rr, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(rr, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(rr, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r2, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, e1, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r2, e2, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r3, e2, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r3, e3, Some(IBgpClient)).unwrap();
        net.set_bgp_session(p1, e1, Some(EBgp)).unwrap();
        net.set_bgp_session(p2, e2, Some(EBgp)).unwrap();
        net.set_bgp_session(p3, e3, Some(EBgp)).unwrap();

        // apply the start configuration
        let net_hist_1 = net.clone();
        net.advertise_external_route(p1, prefix, vec![AsId(1)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(p2, prefix, vec![AsId(1)], None, None)
            .unwrap();
        let net_hist_3 = net.clone();
        net.advertise_external_route(p3, prefix, vec![AsId(1)], None, None)
            .unwrap();

        net.set_msg_limit(Some(5_000));

        // change from the bottom up
        // modify e2
        let net_hist_4 = net.clone();
        assert_eq!(
            net.set_bgp_session(r3, e2, None),
            Err(NetworkError::NoConvergence)
        );

        net.queue.clear();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_4);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[test]
    fn change_ibgp_topology_2<P: Prefix>() {
        // Example from L. Vanbever bgpmig_ton, figure 1
        //
        // igp topology
        //
        // rr is connected to e1, e2, e3 with weights 1, 2, 3 respectively. Assymetric: back direction
        //                               has weight 100
        // ri is connected to ei with weight 10
        // ri is connected to ei-1 with weight 1
        //
        //    _________________
        //  /                  \
        // |  r3       r2       r1
        // |  | '-.    | '-.    |
        //  \ |    '-. |    '-. |
        //    e3       e2       e1   internal
        //    |........|........|............
        //    |        |        |    external
        //    p3       p2       p1
        //
        // ibgp start topology
        // .-----------------------.
        // |   rr   r1   r2   r3   | full mesh
        // '--------^----^---/^----'
        //          |    |.-' |
        //          e1   e2   e3
        //
        // ibgp end topology
        //
        //         .-rr-.
        //        /  |   \
        //       /   |    \
        //      r1   r2   r3
        //      |    |    |
        //      e1   e2   e3

        let mut net: Network<P, _> = Network::default();

        let prefix = P::from(0);

        let rr = net.add_router("rr");
        let r1 = net.add_router("r1");
        let r2 = net.add_router("r2");
        let r3 = net.add_router("r3");
        let e1 = net.add_router("e1");
        let e2 = net.add_router("e2");
        let e3 = net.add_router("e3");
        let p1 = net.add_external_router("p1", AsId(65101));
        let p2 = net.add_external_router("p2", AsId(65102));
        let p3 = net.add_external_router("p3", AsId(65103));

        net.add_link(r1, e1);
        net.add_link(r2, e2);
        net.add_link(r3, e3);
        net.add_link(e1, p1);
        net.add_link(e2, p2);
        net.add_link(e3, p3);
        net.add_link(e1, r2);
        net.add_link(e2, r3);
        net.add_link(e3, r1);
        net.add_link(rr, e1);
        net.add_link(rr, e2);
        net.add_link(rr, e3);

        net.set_link_weight(r1, e1, 10.0).unwrap();
        net.set_link_weight(e1, r1, 10.0).unwrap();
        net.set_link_weight(r2, e2, 10.0).unwrap();
        net.set_link_weight(e2, r2, 10.0).unwrap();
        net.set_link_weight(r3, e3, 10.0).unwrap();
        net.set_link_weight(e3, r3, 10.0).unwrap();
        net.set_link_weight(e1, p1, 1.0).unwrap();
        net.set_link_weight(p1, e1, 1.0).unwrap();
        net.set_link_weight(e2, p2, 1.0).unwrap();
        net.set_link_weight(p2, e2, 1.0).unwrap();
        net.set_link_weight(e3, p3, 1.0).unwrap();
        net.set_link_weight(p3, e3, 1.0).unwrap();
        net.set_link_weight(e1, r2, 1.0).unwrap();
        net.set_link_weight(r2, e1, 1.0).unwrap();
        net.set_link_weight(e2, r3, 1.0).unwrap();
        net.set_link_weight(r3, e2, 1.0).unwrap();
        net.set_link_weight(e3, r1, 1.0).unwrap();
        net.set_link_weight(r1, e3, 1.0).unwrap();
        net.set_link_weight(rr, e1, 1.0).unwrap();
        net.set_link_weight(e1, rr, 100.0).unwrap();
        net.set_link_weight(rr, e2, 2.0).unwrap();
        net.set_link_weight(e2, rr, 100.0).unwrap();
        net.set_link_weight(rr, e3, 3.0).unwrap();
        net.set_link_weight(e3, rr, 100.0).unwrap();
        net.set_bgp_session(rr, r1, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(rr, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(rr, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r2, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r2, r3, Some(IBgpPeer)).unwrap();
        net.set_bgp_session(r1, e1, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r2, e2, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r3, e2, Some(IBgpClient)).unwrap();
        net.set_bgp_session(r3, e3, Some(IBgpClient)).unwrap();
        net.set_bgp_session(p1, e1, Some(EBgp)).unwrap();
        net.set_bgp_session(p2, e2, Some(EBgp)).unwrap();
        net.set_bgp_session(p3, e3, Some(EBgp)).unwrap();

        let net_hist_1 = net.clone();
        net.advertise_external_route(p1, prefix, vec![AsId(1)], None, None)
            .unwrap();
        let net_hist_2 = net.clone();
        net.advertise_external_route(p2, prefix, vec![AsId(1)], None, None)
            .unwrap();
        let net_hist_3 = net.clone();
        net.advertise_external_route(p3, prefix, vec![AsId(1)], None, None)
            .unwrap();

        // change from the middle routers first
        // modify r1
        let net_hist_4 = net.clone();
        net.set_bgp_session(r1, r2, None).unwrap();
        let net_hist_5 = net.clone();
        net.set_bgp_session(r1, r3, None).unwrap();
        let net_hist_6 = net.clone();
        net.set_bgp_session(rr, r1, Some(IBgpClient)).unwrap();

        // modify r2
        let net_hist_7 = net.clone();
        net.set_bgp_session(r2, r3, None).unwrap();
        let net_hist_8 = net.clone();
        net.set_bgp_session(rr, r2, Some(IBgpClient)).unwrap();

        // modify r3
        let net_hist_9 = net.clone();
        net.set_bgp_session(rr, r3, Some(IBgpClient)).unwrap();

        // modify e2
        let net_hist_10 = net.clone();
        net.set_bgp_session(r3, e2, None).unwrap();

        net.undo_action().unwrap();
        assert_eq!(net, net_hist_10);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_9);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_8);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_7);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_6);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_5);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_4);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_3);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_2);
        net.undo_action().unwrap();
        assert_eq!(net, net_hist_1);
    }

    #[instantiate_tests(<SinglePrefix>)]
    mod single {}

    #[instantiate_tests(<SimplePrefix>)]
    mod simple {}

    #[instantiate_tests(<Ipv4Prefix>)]
    mod ipv4 {}
}
