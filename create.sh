pod=$1
ctr=$2

sudo crictl rmp -fa
POD=$(sudo crictl runp --runtime kata-qemu-nvidia-gpu-dev ./${pod})
CTR=$(sudo crictl create "$POD" ./${ctr} ./${pod})
sudo crictl start "$CTR"
sudo crictl exec -it $CTR bash
