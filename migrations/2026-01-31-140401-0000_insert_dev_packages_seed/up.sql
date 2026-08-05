DO $$
DECLARE
    env TEXT;
BEGIN
    SELECT value INTO env FROM app_settings WHERE key = 'env';

    IF env = 'production' THEN
        RAISE NOTICE 'Skipping migration on production';
        RETURN;
    END IF;

    INSERT INTO packages (title, slug, tier, description, price, discount_percent, is_trial, is_active)
    VALUES
        (
            'Free',
            'free-trial',
            1,
            E'- Free plan — **$0** charged every month\n- **$5** credited to your balance each month while you stay on Free\n- Renews automatically when your Free period ends (or claim again after it expires)\n- Up to **2 projects**\n- Up to **1 stage runtime** (plus production)\n- Standard usage rates (no volume discount on compute, storage, or AI tokens)\n- Full platform access to build and deploy',
            5,
            100,
            FALSE,
            TRUE
        ),
        (
            'Solo',
            'solo',
            2,
            E'- Everything in Free, with more room to grow\n- Up to **3 projects**\n- Up to **1 stage runtime** (plus production)\n- Standard usage rates (no volume discount)\n- **$29 / month** — that amount is paid and credited to your balance\n- Annual billing saves an extra **20%** on the plan fee\n- Best for small apps and early production',
            29,
            0,
            FALSE,
            TRUE
        ),
        (
            'Team',
            'team',
            3,
            E'- Everything in Solo, plus lower usage costs\n- Up to **10 projects**\n- Up to **2 stage runtimes** (plus production)\n- **15% off** compute, storage, and AI token usage rates while active\n- Pay **$119 / month**; **$140** credited to your balance (discount bonus is for usage, not withdrawable)\n- Annual billing saves an extra **20%** on the plan fee\n- Best for growing products with steady usage',
            140,
            15,
            FALSE,
            TRUE
        ),
        (
            'Scale',
            'scale',
            4,
            E'- Everything in Team, at maximum scale\n- Up to **40 projects**\n- Up to **5 stage runtimes** (plus production)\n- **30% off** compute, storage, and AI token usage rates while active\n- Pay **$499 / month**; **$713** credited to your balance (discount bonus is for usage, not withdrawable)\n- Annual billing saves an extra **20%** on the plan fee\n- Best for teams and high-volume workloads',
            713,
            30,
            FALSE,
            TRUE
        );
END $$;
