CREATE TABLE IF NOT EXISTS public.user_bindings (
  org_fingerprint varchar(255),
  user_fingerprint varchar(255),
  account_id varchar(255),
  created_at timestamp
)
