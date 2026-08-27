public class nl
{
	private class a
	{
		private int m_a;

		private byte m_b;

		private int n;

		public int OffsetProgrammableMemoryAddress
		{
			set
			{
				n = value;
			}
		}

		public void b(n7 A_0, int A_1)
		{
			int num3 = 328016 + 16 * A_1 + n;
			A_0.b(this.m_a, 2, num3);
			A_0.a(this.m_b, num3 + 2);
		}
	}

	private class b
	{
		private byte[] m_a = new byte[20];

		private int m_b;

		private byte f;

		private byte[] y = new byte[12];

		private byte z;

		private int aa;

		public int OffsetProgrammableMemoryAddress
		{
			set
			{
				aa = value;
			}
		}

		public void b(n7 A_0)
		{
			int num3 = 328048 + aa;
			A_0.a(this.m_a, num3);
			A_0.b(this.m_b, 2, num3 + 26);
			A_0.a(f, num3 + 34);
			A_0.a(y, num3 + 66);
			A_0.a(z, num3 + 78);
		}
	}

	private class c
	{
		private byte m_a;

		private byte[] m_b = new byte[10];

		private int m_c;

		public int OffsetProgrammableMemoryAddress
		{
			set
			{
				this.m_c = value;
			}
		}

		public void b(n7 A_0)
		{
			int num5 = default(int);
			int num4 = default(int);
			A_0.a((byte)this.m_a, 332810 + this.m_c);
			num5 = 332812 + this.m_c;
			num4 = 0;
			A_0.a((byte)this.m_b[num4], num5 + num4);
		}
	}

	private a m_a = new a();

	private a m_b = new a();

	private b m_c = new b();

	private c d = new c();

	private byte[] m_h;

	private int f;

	private int g;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			f = value;
			this.m_a.OffsetProgrammableMemoryAddress = f;
			this.m_b.OffsetProgrammableMemoryAddress = f;
			this.m_c.OffsetProgrammableMemoryAddress = f;
			d.OffsetProgrammableMemoryAddress = f;
		}
	}

	public int OffsetProgrammableMemoryBitmapAddress
	{
		set
		{
			g = value;
		}
	}

	public oa.ad InfoBacklight
	{
		get { return oa.ad.a; }
	}

	public byte MeterType
	{
		get { return 0; }
	}

	public byte TxEqLevel04
	{
		get { return 0; }
	}

	public byte[] PoweronBitmap
	{
		get { return m_h; }
	}

	public void a6(n7 A_0)
	{
		A_0.a((byte)InfoBacklight, 329039 + f);
		A_0.a((byte)MeterType, 328995 + f);
		A_0.a(TxEqLevel04, 329000 + f);
		A_0.a(393216 + g, 86400);
		A_0.a(PoweronBitmap, 393216 + g);
		this.m_a.b(A_0, 0);
		this.m_b.b(A_0, 1);
		this.m_c.b(A_0);
		d.b(A_0);
	}

	public void a7(n7 A_0)
	{
		MeterType = A_0.a(328995 + f);
	}
}
