/** A file record returned by the backend API. */
export interface ServerFile {
  id: string;
  filename: string;
  mime_type: string;
  size_bytes: number;
  uploaded_at: number;
  expires_at: number;
  url: string;
  delete_token: string;
  storage_host?: string;
}
