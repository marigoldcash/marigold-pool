-- Where to look for a payout's transaction later: the change address of the
-- transaction that carries the payments. The pool records it, with the
-- transaction id, before the transaction is broadcast, so that after a crash it
-- can ask the node whether the payment really went out.
ALTER TABLE payouts ADD COLUMN watch_address TEXT;
