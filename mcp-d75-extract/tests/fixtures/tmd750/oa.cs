public class oa
{
	private class bd
	{
		private int m_a;

		private byte m_b;

		public void b(n7 A_0, int A_1)
		{
			int num3 = 848 + 16 * A_1;
			A_0.b(this.m_a, 2, num3);
			A_0.a(this.m_b, num3 + 2);
		}
	}

	private class be
	{
		private int[] m_a = new int[13];

		private byte[] m_b = new byte[53];

		public void b(n7 A_0)
		{
			int num2 = default(int);
			int num4 = default(int);
			num2 = 880;
			num4 = 0;
			A_0.b(this.m_a[num4], 2, num2);
			A_0.a(this.m_b, num2);
		}
	}

	public enum ad : byte
	{
		a,
		b,
	}

	private bd m_a = new bd();

	private bd m_b = new bd();

	private be m_c = new be();

	private List<nl> m_l;

	public byte RepeaterMode
	{
		get { return 0; }
	}

	public string RemoteContorolCode
	{
		get { return string.Empty; }
	}

	public void ai()
	{
		int num3 = 0;
		while (num3 < 6)
		{
			this.m_l.Add(new nl());
			this.m_l[num3].OffsetProgrammableMemoryAddress = 8192 * num3;
			this.m_l[num3].OffsetProgrammableMemoryBitmapAddress = 256000 * num3;
			num3++;
		}
	}

	public void a6(n7 A_0)
	{
		int num3 = 0;
		A_0.a(RepeaterMode, 323585);
		A_0.d(RemoteContorolCode, 4224, oc.e);
		this.m_a.b(A_0, 0);
		this.m_b.b(A_0, 1);
		this.m_c.b(A_0);
		while (num3 < 6)
		{
			this.m_l[num3].a6(A_0);
			num3++;
		}
	}

	public void a7(n7 A_0)
	{
		RepeaterMode = A_0.a(323585);
		this.m_l[0].a7(A_0);
	}
}
