## First launch

This build is for Apple Silicon Macs. Download the `.dmg`, open it and drag Astrolabe to Applications.

Astrolabe is signed ad hoc only, not with an Apple Developer ID, and is not notarized, so macOS blocks the first launch. To allow it:

1. Open Astrolabe from Applications. macOS shows the dialog '"Astrolabe.app" Not Opened'; click Done.
2. Open System Settings > Privacy & Security and scroll down to Security, which reads '"Astrolabe.app" was blocked to protect your Mac.' Click Open Anyway.
3. In the dialog 'Open "Astrolabe.app"?', click Open Anyway, then confirm with Touch ID or an administrator's name and password.

Astrolabe then starts, and opens normally from then on. Right-click > Open does not get past the first dialog: it shows the same "Not Opened" dialog.

Or skip these steps by running:

```sh
xattr -dr com.apple.quarantine /Applications/Astrolabe.app
```

On launch Astrolabe lists the clusters of your kubeconfig (`~/.kube/config`).

## Required permissions

Pending the company AKS validation (#372). This section will list the Kubernetes permissions Astrolabe needs once that issue is resolved.
