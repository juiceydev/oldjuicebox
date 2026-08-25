#!/bin/bash
# Cloudflare WARP egress in an isolated network namespace.
# Host <-> netns via veth (10.200.0.1 <-> 10.200.0.2); tinyproxy inside the
# namespace exposes WARP egress as an HTTP proxy on :8889 (tailnet-reachable).
set -e

CONF=/var/lib/warp/wgcf-profile.conf
NS=warpns

# host-side cleanup if re-running
ip netns del $NS 2>/dev/null || true
ip link del v-warp-host 2>/dev/null || true

ip netns add $NS
ip link add v-warp-host type veth peer name v-warp-ns
ip link set v-warp-ns netns $NS
ip addr add 10.200.0.1/30 dev v-warp-host
ip link set v-warp-host up
ip netns exec $NS ip link set lo up
ip netns exec $NS ip addr add 10.200.0.2/30 dev v-warp-ns
ip netns exec $NS ip link set v-warp-ns up
ip netns exec $NS ip route add default via 10.200.0.1

# NAT the namespace out the host egress via native nftables (iptables nat
# extensions are missing from this kernel, but nft masquerade works).
sysctl -qw net.ipv4.ip_forward=1
EGRESS=$(ip route show default | grep -oP 'dev \K\S+' | head -1)
nft delete table ip warpnat 2>/dev/null || true
nft -f - <<NFT
table ip warpnat {
    chain postrouting {
        type nat hook postrouting priority 100;
        ip saddr 10.200.0.0/30 oifname != "v-warp-host" masquerade
    }
}
NFT

# pin WireGuard endpoint to an IP so nothing needs DNS before resolv is live
EP_IP=$(getent ahostsv4 engage.cloudflareclient.com | awk '{print $1; exit}')
sed -i "s|Endpoint = engage.cloudflareclient.com:2408|Endpoint = ${EP_IP}:2408|" $CONF

# per-namespace DNS (iproute2 binds this over /etc/resolv.conf in-netns)
mkdir -p /etc/netns/$NS
echo "nameserver 1.1.1.1" > /etc/netns/$NS/resolv.conf

# wireguard inside the namespace (manual wg-quick equivalent)
ip netns exec $NS ip link add wgcf type wireguard
ip netns exec $NS wg setconf wgcf <(grep -vE "^(Address|DNS|MTU)\s*=" $CONF)
PRIV=$(grep -oP 'PrivateKey\s*=\s*\K.*' $CONF)
ip netns exec $NS wg set wgcf private-key <(echo "$PRIV")
ip netns exec $NS ip addr add $(grep -oP 'Address\s*=\s*\K[0-9a-f:.]+/32' $CONF | head -1) dev wgcf
ip netns exec $NS ip link set mtu 1280 up dev wgcf
ip netns exec $NS ip route add $(grep -oP 'Endpoint = \K[^:]+' $CONF | head -1)/32 via 10.200.0.1 2>/dev/null || true
ip netns exec $NS ip route replace default dev wgcf
PEER=$(ip netns exec $NS wg show wgcf peers)
ip netns exec $NS wg set wgcf peer "$PEER" persistent-keepalive 25

# tinyproxy inside the namespace, reachable from the host veth address
cat > /etc/warp-proxy.conf <<'TP'
Port 8889
Listen 10.200.0.2
Timeout 600
Allow 10.200.0.0/30
MaxClients 50
TP
ip netns exec $NS tinyproxy -c /etc/warp-proxy.conf

sleep 1
echo "--- egress identity via WARP proxy:"
curl -s --max-time 25 -x http://10.200.0.2:8889 https://api.ipify.org && echo " <- cloudflare"
