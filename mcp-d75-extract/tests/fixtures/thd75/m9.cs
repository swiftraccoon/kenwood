public class m9
{
	public enum a : byte
	{
		a,
		b = 4,
	}

	private class a4
	{
		private int m_a;

		public void b(m6 A_0, int A_1)
		{
			int num3 = 848 + 16 * A_1;
			A_0.b(this.m_a, 2, num3);
		}
	}

	private class a5
	{
		private int[] m_a = new int[13];

		private byte[] m_b = new byte[42];

		public void b(m6 A_0)
		{
			int num2 = default(int);
			int num4 = default(int);
			num2 = 880;
			num4 = 0;
			A_0.b(this.m_a[num4], 2, num2);
			A_0.a(this.m_b, num2);
		}
	}

	private byte[] m_a0;

	private a4 m_a = new a4();

	private a4 m_b = new a4();

	private a5 m_c = new a5();

	public a BeatShift
	{
		get { return a.a; }
	}

	public bool TxInhibit
	{
		get { return false; }
	}

	public bool LedControl_Receive
	{
		get { return false; }
	}

	public string PowerOnMessage
	{
		get { return string.Empty; }
	}

	public byte[] PoweronBitmap
	{
		get { return m_a0; }
	}

	public void a0(m6 A_0)
	{
		A_0.a((byte)BeatShift, 4096);
		A_0.a(TxInhibit, 4097);
		A_0.a((byte)0, 4136);
		A_0.a(Convert.ToByte(LedControl_Receive), 0, 4136);
		A_0.c(PowerOnMessage, 4288, nb.c);
		A_0.a(PoweronBitmap, 327680);
		A_0.a(327680, 86400);
		this.m_a.b(A_0, 0);
		this.m_b.b(A_0, 1);
		this.m_c.b(A_0);
	}

	public void a1(m6 A_0)
	{
		TxInhibit = A_0.a(4097) != 0;
	}
}
