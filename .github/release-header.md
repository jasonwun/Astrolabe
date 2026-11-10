## First launch

This build is for Apple Silicon Macs. Download the `.dmg`, open it and drag Astrolabe to Applications.

Astrolabe is not signed or notarized, so macOS blocks the first launch.
Either right-click Astrolabe in Applications and choose Open, or run:

```sh
xattr -dr com.apple.quarantine /Applications/Astrolabe.app
```

On launch Astrolabe lists the clusters of your kubeconfig (`~/.kube/config`).

## Required permissions

Pending the company AKS validation (#372). This section will list the
Kubernetes permissions Astrolabe needs once that issue is resolved.
