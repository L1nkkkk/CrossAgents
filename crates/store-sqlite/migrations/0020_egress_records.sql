CREATE TABLE egress_records (
    account TEXT NOT NULL,
    egress_ip TEXT,
    asn TEXT,
    country TEXT,
    at TEXT NOT NULL
);
CREATE INDEX egress_records_account_at ON egress_records(account, at);
