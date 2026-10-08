# Kernel lifecycle CPU and RTNL summary

[Read the consolidated PDF](nullnet-cpu-rtnl-breakdown.pdf).

Thirteen pages summarize fresh veth/VXLAN devices and retained pools, including CPU ownership, complete operation tables, RTNL holds, event filters and throughput limits. October 8 bridge-free C256 columns extend the existing tables while preserving historical values. The Next steps page has been removed. Only kernel measurements are included; matched reruns define the reported gains.

Generate with `python3 build_pdf.py` (Python, NumPy and Matplotlib). Charts and tables are vector graphics with searchable text. Source JSON hashes are saved in `source-manifest.json`; raw evidence stays in the corresponding report directories.
