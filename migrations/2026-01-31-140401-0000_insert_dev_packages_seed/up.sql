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
            'Basic',
            'basic',
            2,
            E'- Everything in Free, with more room to grow\n- Up to **3 projects**\n- Up to **1 stage runtime** (plus production)\n- Standard usage rates (no volume discount)\n- Sale price after package discount is what you pay — that same amount is added to your balance\n- Annual billing saves an extra **20%** on the plan fee\n- Best for small apps and early production',
            10,
            10,
            FALSE,
            TRUE
        ),
        (
            'Pro',
            'pro',
            3,
            E'- Everything in Basic, plus lower usage costs\n- Up to **10 projects**\n- Up to **2 stage runtimes** (plus production)\n- **15% off** compute, storage, and AI token usage rates while active\n- Sale price after package discount is what you pay — that same amount is added to your balance\n- Annual billing saves an extra **20%** on the plan fee\n- Best for growing products with steady usage',
            30,
            20,
            FALSE,
            TRUE
        ),
        (
            'Ultra',
            'ultra',
            4,
            E'- Everything in Pro, at maximum scale\n- Up to **40 projects**\n- Up to **5 stage runtimes** (plus production)\n- **30% off** compute, storage, and AI token usage rates while active\n- Sale price after package discount is what you pay — that same amount is added to your balance\n- Annual billing saves an extra **20%** on the plan fee\n- Best for teams and high-volume workloads',
            70,
            0,
            FALSE,
            TRUE
        );
END $$;
