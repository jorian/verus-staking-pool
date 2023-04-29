ALTER TABLE public.latest_state RENAME COLUMN currencyidhex TO currencyid;
ALTER TABLE public.latest_state ADD PRIMARY KEY (currencyid, address);