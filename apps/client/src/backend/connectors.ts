// The connector catalogue: cloud destinations Zeus can deliver to and sources it can read from, with an honest status
// per docs/IMPLEMENTATION_STATUS.md and spec 14. "Built" means a native adapter exists in zeus-cloud or the daemon;
// "Via rclone" means delivery works through an explicit rclone remote the person configures (spec 14: backend-
// dependent, no automatic upload replay); "Manifest only" means the connector is described here and not built.

export type ConnectorStatus = 'built' | 'rclone' | 'manifest'
export type ConnectorRole = 'destination' | 'source' | 'notify'

export interface Connector {
  id: string
  name: string
  role: ConnectorRole
  group: 'Cloud storage' | 'Object storage' | 'File transfer' | 'Web sources' | 'Messaging and notes' | 'Peer to peer'
  status: ConnectorStatus
  auth: string
  hosts: string[]
  summary: string
  /** What works today and what does not, in one or two sentences. */
  note: string
  modes?: string[]
}

export const STATUS_LABEL: Record<ConnectorStatus, string> = { built: 'Built', rclone: 'Via rclone', manifest: 'Manifest only' }

export const CONNECTORS: Connector[] = [
  { id: 'google-drive', name: 'Google Drive', role: 'destination', group: 'Cloud storage', status: 'built', auth: 'PKCE sign-in and refresh in the OS keyring', hosts: ['www.googleapis.com'], summary: 'Deliver verified files to a Drive folder.', modes: ['staged upload', 'bounded streaming (exact length)'], note: 'Native adapter with a persisted resumable session; provider URL fetch is unavailable. Live-account acceptance is pending.' },
  { id: 'yandex-disk', name: 'Yandex Disk', role: 'destination', group: 'Cloud storage', status: 'built', auth: 'PKCE sign-in and refresh in the OS keyring', hosts: ['cloud-api.yandex.net'], summary: 'Upload, or let Yandex fetch a public URL.', modes: ['staged upload', 'bounded streaming', 'provider URL fetch (with weaker-limit consent)'], note: 'Native adapter; provider fetch polls recorded import operations. Live-account acceptance is pending.' },
  { id: 'baidu-netdisk', name: 'Baidu Netdisk', role: 'destination', group: 'Cloud storage', status: 'built', auth: 'Device sign-in and refresh in the OS keyring', hosts: ['pan.baidu.com'], summary: 'Upload with a 4 MiB block manifest and no overwrite.', modes: ['staged upload', 'bounded streaming'], note: 'Native adapter that uploads only missing parts. Live-account acceptance is pending.' },
  { id: 'onedrive', name: 'OneDrive', role: 'destination', group: 'Cloud storage', status: 'rclone', auth: 'rclone remote (credentials stay in rclone.conf)', hosts: ['graph.microsoft.com'], summary: 'Deliver through an rclone remote you configure.', note: 'Works through an explicit rclone remote; restart reconciles the object path, upload replay is unavailable.' },
  { id: 'dropbox', name: 'Dropbox', role: 'destination', group: 'Cloud storage', status: 'rclone', auth: 'rclone remote', hosts: ['api.dropboxapi.com'], summary: 'Deliver through an rclone remote.', note: 'Through an explicit rclone remote; backend-dependent behaviour.' },
  { id: 'box', name: 'Box', role: 'destination', group: 'Cloud storage', status: 'rclone', auth: 'rclone remote', hosts: ['api.box.com'], summary: 'Deliver through an rclone remote.', note: 'Through an explicit rclone remote; backend-dependent behaviour.' },
  { id: 'pcloud', name: 'pCloud', role: 'destination', group: 'Cloud storage', status: 'rclone', auth: 'rclone remote', hosts: ['api.pcloud.com'], summary: 'Deliver through an rclone remote.', note: 'Through an explicit rclone remote; backend-dependent behaviour.' },
  { id: 'mega', name: 'MEGA', role: 'destination', group: 'Cloud storage', status: 'rclone', auth: 'rclone remote', hosts: ['g.api.mega.co.nz'], summary: 'Deliver through an rclone remote.', note: 'Spec 14 names MEGA among the supported rclone backends.' },
  { id: 's3', name: 'Amazon S3', role: 'destination', group: 'Object storage', status: 'rclone', auth: 'rclone remote (access key in rclone.conf)', hosts: ['s3.amazonaws.com'], summary: 'Deliver objects to a bucket through rclone.', note: 'Through an explicit rclone remote with an exact length; no provider URL fetch.' },
  { id: 'r2', name: 'Cloudflare R2', role: 'destination', group: 'Object storage', status: 'rclone', auth: 'rclone remote (S3 API)', hosts: ['<account>.r2.cloudflarestorage.com'], summary: 'S3-compatible bucket through rclone.', note: 'Through an explicit rclone remote.' },
  { id: 'gcs', name: 'Google Cloud Storage', role: 'destination', group: 'Object storage', status: 'rclone', auth: 'rclone remote (service account file)', hosts: ['storage.googleapis.com'], summary: 'Bucket delivery through rclone.', note: 'Through an explicit rclone remote.' },
  { id: 'azure-blob', name: 'Azure Blob Storage', role: 'destination', group: 'Object storage', status: 'rclone', auth: 'rclone remote (SAS or key in rclone.conf)', hosts: ['<account>.blob.core.windows.net'], summary: 'Container delivery through rclone.', note: 'Through an explicit rclone remote.' },
  { id: 'b2', name: 'Backblaze B2', role: 'destination', group: 'Object storage', status: 'rclone', auth: 'rclone remote', hosts: ['api.backblazeb2.com'], summary: 'Bucket delivery through rclone.', note: 'Through an explicit rclone remote.' },
  { id: 'wasabi', name: 'Wasabi', role: 'destination', group: 'Object storage', status: 'rclone', auth: 'rclone remote (S3 API)', hosts: ['s3.wasabisys.com'], summary: 'S3-compatible bucket through rclone.', note: 'Through an explicit rclone remote.' },
  { id: 'webdav', name: 'WebDAV', role: 'destination', group: 'File transfer', status: 'rclone', auth: 'rclone remote', hosts: ['your server'], summary: 'Deliver to a WebDAV share through rclone.', note: 'Through an explicit rclone remote.' },
  { id: 'sftp', name: 'SFTP', role: 'destination', group: 'File transfer', status: 'rclone', auth: 'rclone remote (SSH key path in rclone.conf)', hosts: ['your server'], summary: 'Deliver over SSH through rclone.', note: 'Through an explicit rclone remote. Zeus itself downloads http and https only.' },
  { id: 'ftp', name: 'FTP', role: 'destination', group: 'File transfer', status: 'rclone', auth: 'rclone remote', hosts: ['your server'], summary: 'Deliver to an FTP server through rclone.', note: 'Delivery only, through rclone. Downloading from ftp:// addresses is refused.' },
  { id: 'github-releases', name: 'GitHub Releases', role: 'source', group: 'Web sources', status: 'manifest', auth: 'None for public assets', hosts: ['github.com', 'objects.githubusercontent.com'], summary: 'List a release and pick its assets.', note: 'Public release assets are ordinary https downloads and work today. Listing a release\'s assets as a source is not built.' },
  { id: 's3-source', name: 'Amazon S3 downloads', role: 'source', group: 'Object storage', status: 'built', auth: 'Explicit AWS credentials in the OS keyring', hosts: ['s3.amazonaws.com'], summary: 'Download versioned objects within one bucket and key prefix.', note: 'Connected sources pins object identity before segmented download and rechecks account authority at publication. Native AWS S3 endpoints only; live-account acceptance is pending.' },
  { id: 'hugging-face', name: 'Hugging Face', role: 'source', group: 'Web sources', status: 'built', auth: 'Explicit optional token in the OS keyring', hosts: ['huggingface.co'], summary: 'Download exact files from a pinned repository commit.', note: 'Connected sources reviews a full commit and path within one configured repository. Live gated-account acceptance is pending.' },
  { id: 'media-pages', name: 'Media pages via the extension', role: 'source', group: 'Web sources', status: 'manifest', auth: 'Your browser session, after you approve the site', hosts: ['video and audio pages'], summary: 'Hand a media page\'s file from the browser to Zeus.', note: 'The extension hands over downloads the browser starts. Extracting streams from a media page is not built, and Zeus does not bypass protection.' },
  { id: 'rss', name: 'RSS and Atom feeds', role: 'source', group: 'Web sources', status: 'manifest', auth: 'None', hosts: ['any feed URL'], summary: 'Watch a feed and queue new enclosures.', note: 'Workflows read HTML and JSON pages today; feed parsing and watching are not built.' },
  { id: 'ipfs', name: 'IPFS gateway', role: 'source', group: 'Web sources', status: 'manifest', auth: 'None', hosts: ['a gateway you choose'], summary: 'Fetch content addressed files through an https gateway.', note: 'A gateway URL is an ordinary https download. Content-address verification against the CID is not built.' },
  { id: 'telegram', name: 'Telegram', role: 'notify', group: 'Messaging and notes', status: 'manifest', auth: 'Bot token in the OS keyring', hosts: ['api.telegram.org'], summary: 'Send a message when a batch finishes.', note: 'Not built.' },
  { id: 'discord', name: 'Discord', role: 'notify', group: 'Messaging and notes', status: 'manifest', auth: 'Webhook URL in the OS keyring', hosts: ['discord.com'], summary: 'Post to a channel when a batch finishes.', note: 'Not built.' },
  { id: 'slack', name: 'Slack', role: 'notify', group: 'Messaging and notes', status: 'manifest', auth: 'Webhook URL in the OS keyring', hosts: ['hooks.slack.com'], summary: 'Post to a channel when a batch finishes.', note: 'Not built.' },
  { id: 'notion', name: 'Notion', role: 'notify', group: 'Messaging and notes', status: 'manifest', auth: 'Integration token in the OS keyring', hosts: ['api.notion.com'], summary: 'Record finished downloads in a database.', note: 'Not built.' },
  { id: 'torrent', name: 'Torrent files', role: 'source', group: 'Peer to peer', status: 'built', auth: 'Explicit source scope and peer IP allowlist', hosts: ['peers'], summary: 'Acquire selected torrent files with receive and time limits.', note: 'Requires .torrent metainfo and explicit peer addresses. Magnet, tracker and DHT discovery are unavailable; completed files are not seeded.' },
]
